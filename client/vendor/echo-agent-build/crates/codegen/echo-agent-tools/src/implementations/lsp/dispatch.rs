//! Bridges echo-agent-tools LspBackend trait to LspManager.
//!
//! `dispatch_on_sockets` is a thin router; each LSP operation has its own helper.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex as TokioMutex;

use async_lsp::LanguageServer;
use async_lsp::lsp_types::{
    self, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
    GotoDefinitionResponse, HoverParams, Location, ReferenceContext, ReferenceParams,
    SymbolInformation, TextDocumentIdentifier, TextDocumentPositionParams, WorkspaceSymbolParams,
};

use super::{LspToolInput, LspToolResult};

use super::config::REQUEST_TIMEOUT;
use super::format::{
    flatten_document_symbols, format_locations_labeled, format_symbols, markup_string_to_text,
};
use super::manager::LspManager;
use super::{LspError, file_uri, text_document_position};

// ── Public adapter ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum StartupState {
    NotStarted,
    Starting,
    Ready,
    Failed(String),
}

struct StartupCoordinator {
    state: TokioMutex<StartupState>,
    notify: tokio::sync::Notify,
    pre_ready_file_changes: TokioMutex<HashMap<PathBuf, String>>,
}

pub struct LspBackendAdapter {
    lsp_manager: Arc<tokio::sync::Mutex<LspManager>>,
    startup: Arc<StartupCoordinator>,
}

impl LspBackendAdapter {
    pub fn new(lsp_manager: Arc<tokio::sync::Mutex<LspManager>>) -> Self {
        Self {
            lsp_manager,
            startup: Arc::new(StartupCoordinator {
                state: TokioMutex::new(StartupState::NotStarted),
                notify: tokio::sync::Notify::new(),
                pre_ready_file_changes: TokioMutex::new(HashMap::new()),
            }),
        }
    }

    fn spawn_bootstrap_task(
        lsp_manager: Arc<tokio::sync::Mutex<LspManager>>,
        startup: Arc<StartupCoordinator>,
    ) {
        tokio::spawn(async move {
            let result = bootstrap_lsp(lsp_manager, startup.clone()).await;
            let mut state = startup.state.lock().await;
            *state = match result {
                Ok(()) => StartupState::Ready,
                Err(error) => StartupState::Failed(error),
            };
            drop(state);
            startup.notify.notify_waiters();
        });
    }

    async fn ensure_started_inner(&self) {
        Self::ensure_started_with_state(self.lsp_manager.clone(), self.startup.clone()).await;
    }

    async fn ensure_started_with_state(
        lsp_manager: Arc<tokio::sync::Mutex<LspManager>>,
        startup: Arc<StartupCoordinator>,
    ) {
        let mut state = startup.state.lock().await;
        if matches!(&*state, StartupState::NotStarted) {
            *state = StartupState::Starting;
            drop(state);
            LspBackendAdapter::spawn_bootstrap_task(lsp_manager, startup);
        }
    }
}

async fn bootstrap_lsp(
    lsp_manager: Arc<tokio::sync::Mutex<LspManager>>,
    startup: Arc<StartupCoordinator>,
) -> Result<(), String> {
    let pending_changes: Vec<(PathBuf, String)> = {
        let mut pending = startup.pre_ready_file_changes.lock().await;
        pending.drain().collect()
    };

    let restartable = {
        let mut mgr = lsp_manager.lock().await;
        mgr.ensure_initialized().await;
        if mgr.clients.is_empty() {
            return Err("No LSP servers started successfully.".to_string());
        }
        for (path, content) in &pending_changes {
            mgr.notify_file_changed(path, content);
        }
        mgr.restartable_servers()
    };
    for name in restartable {
        // Hand the monitor a `Weak` so it never keeps the manager (and its
        // language-server children) alive past the owning session.
        let mgr_weak = Arc::downgrade(&lsp_manager);
        tokio::spawn(crate::implementations::lsp::restart_monitor(mgr_weak, name));
    }
    Ok(())
}

impl Drop for LspBackendAdapter {
    fn drop(&mut self) {
        tracing::debug!("LspBackendAdapter dropping, initiating LSP shutdown");
        let mgr = self.lsp_manager.clone();
        let _ = tokio::runtime::Handle::try_current().map(|handle| {
            handle.spawn(async move {
                mgr.lock().await.shutdown().await;
            })
        });
    }
}

#[async_trait::async_trait]
impl super::LspBackend for LspBackendAdapter {
    fn ensure_started_background(&self) {
        let lsp_manager = self.lsp_manager.clone();
        let startup = self.startup.clone();
        tokio::spawn(async move {
            LspBackendAdapter::ensure_started_with_state(lsp_manager, startup).await;
        });
    }

    async fn ensure_ready(&self) -> Result<(), String> {
        self.ensure_started_inner().await;
        loop {
            let notified = {
                let state = self.startup.state.lock().await;
                match &*state {
                    StartupState::Ready => return Ok(()),
                    StartupState::Failed(error) => return Err(error.clone()),
                    StartupState::NotStarted => continue,
                    StartupState::Starting => self.startup.notify.notified(),
                }
            };
            notified.await;
        }
    }

    fn is_ready(&self) -> bool {
        self.startup
            .state
            .try_lock()
            .map(|state| matches!(&*state, StartupState::Ready))
            .unwrap_or(false)
    }

    async fn dispatch(&self, input: &LspToolInput) -> LspToolResult {
        use super::LspOperation;

        if let Err(error) = self.ensure_ready().await {
            return err_result(format!("LSP startup failed: {error}"));
        }

        // Brief lock: auto-open + clone socket(s). Then drop lock.
        let sockets = {
            let mut mgr = self.lsp_manager.lock().await;
            match input.operation {
                LspOperation::WorkspaceSymbol => {
                    if mgr.clients.is_empty() {
                        return err_result("No LSP servers are running.".into());
                    }
                    DispatchSockets::All(mgr.all_sockets())
                }
                _ => {
                    let path = match input.file_path.as_deref() {
                        Some(fp) => PathBuf::from(fp),
                        None => return err_result("Required: file_path.".into()),
                    };
                    match mgr.socket_for_file(&path).await {
                        Some(s) => DispatchSockets::One(s),
                        None => {
                            return err_result(format!(
                                "No LSP server configured for {}",
                                path.display()
                            ));
                        }
                    }
                }
            }
        };
        // Lock dropped — dispatch on cloned socket(s).
        dispatch_on_sockets(input, sockets).await
    }

    async fn drain_diagnostics(
        &self,
        timeout: std::time::Duration,
    ) -> Option<super::DiagnosticsSummary> {
        if !self.is_ready() {
            return None;
        }
        super::manager::drain_lsp_diagnostics(&self.lsp_manager, timeout).await
    }

    async fn notify_file_changed(&self, path: &std::path::Path, content: &str) {
        if self.is_ready() {
            self.lsp_manager
                .lock()
                .await
                .notify_file_changed(path, content);
        } else {
            self.startup
                .pre_ready_file_changes
                .lock()
                .await
                .insert(path.to_path_buf(), content.to_string());
        }
    }

    async fn read_diagnostics(
        &self,
        paths: &[std::path::PathBuf],
    ) -> Vec<super::FileDiagnosticEntry> {
        if self.ensure_ready().await.is_err() {
            return vec![];
        }

        // Open files that aren't tracked yet so the LSP can analyze them.
        {
            let mut mgr = self.lsp_manager.lock().await;
            for path in paths {
                let _ = mgr.socket_for_file(path).await;
            }
        }

        // Wait briefly after opening files. This is the native-LSP analogue of
        // the IDE wait between TrackModel and the second diagnostics call:
        // opening/tracking a file starts analysis, while diagnostics arrive
        // later through publishDiagnostics.
        let notify = {
            let mgr = self.lsp_manager.lock().await;
            mgr.diagnostics_ready.clone()
        };
        let notified = notify.notified();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(1000), notified).await;

        // Collect diagnostics from all clients for the requested paths.
        let mgr = self.lsp_manager.lock().await;
        let mut results = Vec::new();
        for path in paths {
            let uri = match super::file_uri(path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let uri_str = uri.to_string();
            let mut file_diagnostics = Vec::new();

            for client in mgr.clients.values() {
                for d in client.diagnostics.items(&uri_str) {
                    let severity = match d.severity {
                        Some(async_lsp::lsp_types::DiagnosticSeverity::ERROR) => {
                            super::DiagnosticSeverityLevel::Error
                        }
                        Some(async_lsp::lsp_types::DiagnosticSeverity::WARNING) => {
                            super::DiagnosticSeverityLevel::Warning
                        }
                        _ => continue,
                    };
                    file_diagnostics.push(super::DiagnosticEntry {
                        severity,
                        // LSP uses 0-based positions; convert to 1-based
                        // for display (L{line}:{column}).
                        line: d.range.start.line + 1,
                        column: d.range.start.character + 1,
                        message: d.message.clone(),
                        source: d.source.clone(),
                        code: None,
                        is_stale: false,
                    });
                }
            }

            results.push(super::FileDiagnosticEntry {
                path: path.display().to_string(),
                diagnostics: file_diagnostics,
            });
        }
        results
    }
}

// ── Internal types ──────────────────────────────────────────────────────

enum DispatchSockets {
    One(async_lsp::ServerSocket),
    All(Vec<async_lsp::ServerSocket>),
}

fn err_result(msg: String) -> LspToolResult {
    LspToolResult {
        text: msg,
        is_error: true,
    }
}

fn ok_result(text: String) -> LspToolResult {
    LspToolResult {
        text,
        is_error: false,
    }
}

fn require_position(input: &LspToolInput) -> Result<(PathBuf, TextDocumentPositionParams), String> {
    let (Some(fp), Some(line), Some(character)) = (&input.file_path, input.line, input.character)
    else {
        return Err("Required: file_path, line, character.".into());
    };
    let path = PathBuf::from(fp);
    let params = text_document_position(&path, line, character).map_err(|e| format!("{e}"))?;
    Ok((path, params))
}

/// Awaits an LSP request future with a standard timeout and error mapping.
async fn timed_request<F, T, E>(fut: F) -> Result<T, LspError>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    tokio::time::timeout(REQUEST_TIMEOUT, fut)
        .await
        .map_err(|_| LspError::RequestFailed("request timed out".into()))?
        .map_err(|e| LspError::RequestFailed(format!("{e}")))
}

/// Distinguishes validation errors (missing params) from LSP protocol errors.
enum DispatchError {
    /// Missing or invalid input parameters.
    Validation(String),
    /// LSP request failed or timed out.
    Lsp(LspError),
}

impl From<String> for DispatchError {
    fn from(msg: String) -> Self {
        Self::Validation(msg)
    }
}

impl From<LspError> for DispatchError {
    fn from(e: LspError) -> Self {
        Self::Lsp(e)
    }
}

// ── Router ──────────────────────────────────────────────────────────────

async fn dispatch_on_sockets(input: &LspToolInput, sockets: DispatchSockets) -> LspToolResult {
    use super::LspOperation;

    match sockets {
        DispatchSockets::One(mut socket) => {
            let result = match input.operation {
                LspOperation::GoToDefinition => dispatch_goto(input, &mut socket, true).await,
                LspOperation::GoToImplementation => dispatch_goto(input, &mut socket, false).await,
                LspOperation::FindReferences => dispatch_references(input, &mut socket).await,
                LspOperation::Hover => dispatch_hover(input, &mut socket).await,
                LspOperation::DocumentSymbol => dispatch_document_symbols(input, &mut socket).await,
                LspOperation::WorkspaceSymbol => unreachable!(),
            };
            match result {
                Ok(text) => ok_result(text),
                Err(DispatchError::Validation(msg)) => err_result(msg),
                Err(DispatchError::Lsp(e)) => {
                    tracing::warn!(error = %e, "LSP tool failed");
                    err_result(format!("LSP error: {e}"))
                }
            }
        }
        DispatchSockets::All(sockets) => dispatch_workspace_symbols(input, sockets).await,
    }
}

// ── Per-operation helpers ───────────────────────────────────────────────

/// Handles both GoToDefinition and GoToImplementation (same params/response shape).
async fn dispatch_goto(
    input: &LspToolInput,
    socket: &mut async_lsp::ServerSocket,
    is_definition: bool,
) -> Result<String, DispatchError> {
    let (_path, pos) = require_position(input)?;
    let params = GotoDefinitionParams {
        text_document_position_params: pos,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let label = if is_definition {
        "Definition"
    } else {
        "Implementations"
    };
    let fut = if is_definition {
        socket.definition(params)
    } else {
        socket.implementation(params)
    };
    let response = timed_request(fut).await?;
    let locs = parse_goto_response(response);
    Ok(format_locations_labeled(label, &locs))
}

async fn dispatch_references(
    input: &LspToolInput,
    socket: &mut async_lsp::ServerSocket,
) -> Result<String, DispatchError> {
    let (_path, pos) = require_position(input)?;
    let params = ReferenceParams {
        text_document_position: pos,
        context: ReferenceContext {
            include_declaration: true,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let result = timed_request(socket.references(params)).await?;
    Ok(format_locations_labeled(
        "References",
        &result.unwrap_or_default(),
    ))
}

async fn dispatch_hover(
    input: &LspToolInput,
    socket: &mut async_lsp::ServerSocket,
) -> Result<String, DispatchError> {
    let (_path, pos) = require_position(input)?;
    let params = HoverParams {
        text_document_position_params: pos,
        work_done_progress_params: Default::default(),
    };
    let result = timed_request(socket.hover(params)).await?;
    Ok(result
        .map(|h| match h.contents {
            lsp_types::HoverContents::Scalar(ms) => markup_string_to_text(ms),
            lsp_types::HoverContents::Array(arr) => arr
                .into_iter()
                .map(markup_string_to_text)
                .collect::<Vec<_>>()
                .join("\n"),
            lsp_types::HoverContents::Markup(mc) => mc.value,
        })
        .unwrap_or_else(|| "No hover information available.".to_string()))
}

async fn dispatch_document_symbols(
    input: &LspToolInput,
    socket: &mut async_lsp::ServerSocket,
) -> Result<String, DispatchError> {
    let Some(ref fp) = input.file_path else {
        return Err(DispatchError::Validation("Required: file_path.".into()));
    };
    let uri = file_uri(Path::new(fp))?;
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let result = timed_request(socket.document_symbol(params)).await?;
    Ok(match result {
        Some(DocumentSymbolResponse::Flat(s)) => format_symbols(&s),
        Some(DocumentSymbolResponse::Nested(n)) => {
            let mut flat = Vec::new();
            flatten_document_symbols(&n, &uri, &mut flat);
            format_symbols(&flat)
        }
        None => format_symbols(&[]),
    })
}

async fn dispatch_workspace_symbols(
    input: &LspToolInput,
    sockets: Vec<async_lsp::ServerSocket>,
) -> LspToolResult {
    let Some(ref query) = input.query else {
        return err_result("Required: query (string).".into());
    };
    let mut all_symbols = Vec::new();
    let mut last_err: Option<String> = None;
    for mut s in sockets {
        let params = WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, s.symbol(params)).await {
            Ok(Ok(Some(lsp_types::WorkspaceSymbolResponse::Flat(symbols)))) => {
                all_symbols.extend(symbols)
            }
            Ok(Ok(Some(lsp_types::WorkspaceSymbolResponse::Nested(ws)))) => {
                all_symbols.extend(ws.into_iter().map(workspace_symbol_to_info));
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => last_err = Some(format!("{e}")),
            Err(_) => last_err = Some("request timed out".into()),
        }
    }
    if all_symbols.is_empty() {
        match last_err {
            Some(msg) => err_result(format!("LSP error: {msg}")),
            None => ok_result(format_symbols(&[])),
        }
    } else {
        ok_result(format_symbols(&all_symbols))
    }
}

// ── Response converters ─────────────────────────────────────────────────

fn parse_goto_response(response: Option<GotoDefinitionResponse>) -> Vec<Location> {
    match response {
        Some(GotoDefinitionResponse::Scalar(l)) => vec![l],
        Some(GotoDefinitionResponse::Array(l)) => l,
        Some(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect(),
        None => vec![],
    }
}

fn workspace_symbol_to_info(ws: lsp_types::WorkspaceSymbol) -> SymbolInformation {
    let loc = match ws.location {
        lsp_types::OneOf::Left(l) => l,
        lsp_types::OneOf::Right(d) => Location {
            uri: d.uri,
            range: Default::default(),
        },
    };
    #[allow(deprecated)]
    SymbolInformation {
        name: ws.name,
        kind: ws.kind,
        tags: ws.tags,
        deprecated: None,
        location: loc,
        container_name: ws.container_name,
    }
}
