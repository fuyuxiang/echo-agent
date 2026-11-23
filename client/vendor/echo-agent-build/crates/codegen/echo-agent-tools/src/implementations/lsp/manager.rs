//! Manages multiple LSP servers, routes by file extension, collects diagnostics.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

use super::client::LspClient;
use super::config::LspServerConfig;
use super::pending::{PendingEdits, PendingPolicy};
use super::{DiagnosticsNotify, file_uri};
use crate::util::ProcessScope;

#[cfg(test)]
use super::format::{format_locations_labeled, format_symbols};

use super::types::DiagnosticsSummary;

/// How many diagnostics one file may contribute to a summary.
///
/// A file with forty errors is usually one mistake seen forty times, and the
/// fortieth line teaches the reader nothing the first ten did not.
const MAX_PER_FILE: usize = 10;

/// How many a whole summary may carry.
///
/// A refresh re-opens every document at once, so without a ceiling the first
/// one on a large solution could put every problem in the workspace into a
/// single tool result.
const MAX_PER_SUMMARY: usize = 30;

/// What a drain found: the lines to show, and the counts that go with them.
#[derive(Default)]
struct CollectedDiagnostics {
    lines: Vec<String>,
    file_count: usize,
    /// Reportable diagnostics found, including any the caps left out — the
    /// counts describe what the servers said, not what survived the trim.
    diagnostic_count: usize,
    shown: usize,
}

/// One server's open documents, to be asked about again after it said its
/// answers were out of date.
struct Reopened {
    server_name: String,
    lifecycle_id: u64,
    documents: Vec<(String, i32)>,
}

impl CollectedDiagnostics {
    /// Add the reportable diagnostics for one file. A file with nothing worth
    /// showing — clean, or only hints and information — adds no header.
    ///
    /// Errors come before warnings, and both are capped, so what survives a
    /// trim is the part worth reading. The line each was reported on breaks
    /// ties, so the order does not depend on how the server happened to sort
    /// them.
    fn append_file(&mut self, uri: &str, items: Vec<Diagnostic>) {
        let mut reportable: Vec<(&str, &Diagnostic)> = items
            .iter()
            .filter_map(|d| match d.severity {
                Some(DiagnosticSeverity::ERROR) => Some(("error", d)),
                Some(DiagnosticSeverity::WARNING) => Some(("warn", d)),
                _ => None,
            })
            .collect();
        reportable.sort_by_key(|(label, d)| (*label != "error", d.range.start.line));
        self.diagnostic_count += reportable.len();

        // How many this file may contribute, given what the summary has room
        // for. Applying it here rather than counting inside the loop means the
        // header is only written when something follows it.
        let room = MAX_PER_FILE.min(MAX_PER_SUMMARY.saturating_sub(self.shown));
        let showing: Vec<_> = reportable.into_iter().take(room).collect();
        if showing.is_empty() {
            return;
        }

        let display_path = uri.strip_prefix("file://").unwrap_or(uri); // Unix-only
        self.lines.push(format!("{display_path}:"));
        self.file_count += 1;
        self.shown += showing.len();

        for (label, d) in showing {
            let msg = d
                .message
                .replace("</lsp-diagnostics>", "&lt;/lsp-diagnostics&gt;")
                .replace("</system-reminder>", "&lt;/system-reminder&gt;");
            self.lines
                .push(format!("  {label}[L{}]: {msg}", d.range.start.line + 1));
        }
    }

    /// The line that tells the reader something was left out, if anything was.
    ///
    /// Silently truncating would be worse than not reporting at all: the reader
    /// would take a partial list for the whole truth and conclude the rest of
    /// the file was fine.
    fn trimmed_note(&self) -> Option<String> {
        let hidden = self.diagnostic_count.saturating_sub(self.shown);
        (hidden > 0).then(|| format!("… and {hidden} more not shown"))
    }
}

pub struct LspManager {
    pub servers: BTreeMap<String, LspServerConfig>,
    pub clients: HashMap<String, LspClient>,
    pub workspace_root: PathBuf,
    pub initialized: bool,
    pub tools_enabled: bool,
    pub pending_diagnostics_by_server: HashMap<String, PendingEdits>,
    /// How long a file is held waiting for a verdict, and how long a silent
    /// server is blocked on. Configurable so a test can exercise the real drain
    /// without spending the production durations waiting.
    pub pending_policy: PendingPolicy,
    pub diagnostics_ready: DiagnosticsNotify,
    pub shutting_down: bool,
    pub next_lifecycle_id: u64,
    pub notification_handle: crate::notification::ToolNotificationHandle,
    /// When set, each spawned server's process group is registered here so the
    /// agent can reap the language-server trees on session close. `None` outside
    /// an agent session.
    pub process_scope: Option<ProcessScope>,
}

impl Default for LspManager {
    fn default() -> Self {
        Self {
            servers: BTreeMap::new(),
            clients: HashMap::new(),
            workspace_root: PathBuf::from("/tmp"),
            initialized: false,
            tools_enabled: false,
            pending_diagnostics_by_server: HashMap::new(),
            pending_policy: PendingPolicy::default(),
            diagnostics_ready: Arc::new(tokio::sync::Notify::new()),
            shutting_down: false,
            next_lifecycle_id: 1,
            notification_handle: crate::notification::ToolNotificationHandle::noop(),
            process_scope: None,
        }
    }
}

impl LspManager {
    pub fn new(
        servers: BTreeMap<String, LspServerConfig>,
        workspace_root: PathBuf,
        tools_enabled: bool,
        notification_handle: crate::notification::ToolNotificationHandle,
    ) -> Self {
        Self {
            servers,
            workspace_root,
            tools_enabled,
            notification_handle,
            ..Self::default()
        }
    }

    /// Attach the session process scope so spawned servers are enrolled for
    /// reclaim.
    pub fn with_process_scope(mut self, scope: Option<ProcessScope>) -> Self {
        self.process_scope = scope;
        self
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn alloc_lifecycle_id(&mut self) -> u64 {
        let lifecycle_id = self.next_lifecycle_id;
        self.next_lifecycle_id = self.next_lifecycle_id.saturating_add(1);
        lifecycle_id
    }

    /// Start waiting for the server's verdict on `uri`.
    ///
    /// `version` is the document version the change was sent as — a verdict on
    /// that version or a later one settles it, and one on an earlier version
    /// does not. See [`PendingEdits`].
    pub fn mark_uri_pending_diagnostics(
        &mut self,
        server_name: &str,
        lifecycle_id: u64,
        uri: Url,
        version: i32,
    ) {
        let policy = self.pending_policy;
        self.pending_diagnostics_by_server
            .entry(server_name.to_string())
            .or_insert_with(|| PendingEdits::new(policy))
            .mark(lifecycle_id, uri.as_str(), version, Instant::now());
    }

    pub async fn ensure_initialized(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;

        let configs: Vec<_> = self
            .servers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (name, server_config) in configs {
            self.notification_handle
                .send_lsp_starting(crate::notification::LspServerStarting {
                    server_name: name.clone(),
                    command: server_config.command.clone(),
                });
            let lifecycle_id = self.alloc_lifecycle_id();
            match LspClient::start(
                name.clone(),
                lifecycle_id,
                server_config,
                &self.workspace_root,
                self.diagnostics_ready.clone(),
            )
            .await
            {
                Ok(mut client) => {
                    if !client.enroll(self.process_scope.as_ref()) {
                        // Session teardown raced this start: the closed scope
                        // killed the child at registration. Installing the
                        // client would advertise a dead server and feed the
                        // restart monitor respawn churn, so stop starting
                        // servers for this manager instead.
                        tracing::info!(server = %name, "session scope closed during LSP start; discarding server");
                        self.shutting_down = true;
                        return;
                    }
                    // Same race as the restart path: `kill_all` landing between
                    // enroll and insert has already SIGKILLed the child.
                    if self.process_scope.as_ref().is_some_and(|s| s.is_closed()) {
                        tracing::info!(server = %name, "session scope closed during LSP start; discarding server");
                        self.shutting_down = true;
                        return;
                    }
                    tracing::info!(server = %name, "LSP server ready");
                    self.notification_handle
                        .send_lsp_ready(crate::notification::LspServerReady {
                            server_name: name.clone(),
                        });
                    self.clients.insert(name, client);
                }
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "failed to start LSP server, skipping");
                    self.notification_handle.send_lsp_failed(
                        crate::notification::LspServerFailed {
                            server_name: name.clone(),
                            error: e.to_string(),
                            attempts: 0,
                        },
                    );
                }
            }
        }
    }

    pub async fn shutdown(&mut self) {
        self.shutting_down = true;
        for (name, client) in self.clients.drain() {
            tracing::info!(server = %name, "shutting down LSP server");
            client.shutdown().await;
        }
    }

    pub fn tools_enabled(&self) -> bool {
        self.tools_enabled && !self.clients.is_empty()
    }

    pub fn restartable_servers(&self) -> Vec<String> {
        self.servers
            .iter()
            .filter(|(name, cfg)| cfg.restart_on_crash() && self.clients.contains_key(*name))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Only called after `search_replace`; other mutations (bash, git) are not tracked.
    pub fn notify_file_changed(&mut self, path: &Path, content: &str) {
        let (server_name, lang_id) = match super::config::resolve_server(&self.servers, path) {
            Some(pair) => pair,
            None => return,
        };
        let Ok(uri) = file_uri(path) else {
            return;
        };
        let client = match self.clients.get_mut(&server_name) {
            Some(c) => c,
            None => return,
        };
        let lifecycle_id = client.lifecycle_id;
        // No version means the server was never told about the edit, so there
        // is no verdict on it to wait for.
        let Some(version) = client.notify_file_change(path, content, &lang_id) else {
            return;
        };
        self.mark_uri_pending_diagnostics(&server_name, lifecycle_id, uri, version);
    }

    pub fn has_pending_diagnostics(&self) -> bool {
        self.pending_diagnostics_by_server
            .values()
            .any(|pending| !pending.is_empty())
    }

    /// Whether any pending server is still expected to answer. False once every
    /// server owing us one has been silent for longer than
    /// [`super::pending::SERVER_PATIENCE`], which lets the drain return immediately
    /// instead of blocking for its whole timeout.
    fn worth_blocking_for_diagnostics(&self) -> bool {
        let now = Instant::now();
        self.pending_diagnostics_by_server
            .values()
            .any(|pending| pending.worth_blocking(now))
    }

    /// Ask again about every open document of any server that has told us its
    /// answers are out of date.
    ///
    /// The re-pull is already under way — [`super::refresh`] starts it — but a
    /// document nobody is waiting on has nowhere to report to, so the questions
    /// have to be re-opened as well. Without this, the truth a server arrives
    /// at *after* answering too early would sit in the store until the next
    /// time that file happened to be edited.
    fn reopen_refreshed_questions(&mut self) {
        let mut reopened = Vec::new();
        for (name, client) in &self.clients {
            if client.refresh.take_invalidated() {
                reopened.push(Reopened {
                    server_name: name.clone(),
                    lifecycle_id: client.lifecycle_id,
                    documents: client.documents.versions(),
                });
            }
        }

        let now = Instant::now();
        let policy = self.pending_policy;
        for Reopened {
            server_name,
            lifecycle_id,
            documents,
        } in reopened
        {
            if documents.is_empty() {
                continue;
            }
            tracing::debug!(
                server = %server_name, documents = documents.len(),
                "server's answers were invalidated; waiting on all of its open documents again"
            );
            let pending = self
                .pending_diagnostics_by_server
                .entry(server_name)
                .or_insert_with(|| PendingEdits::new(policy));
            // Asking for a refresh is the server speaking. A server that spent
            // a long time loading has been written off as silent by now, and
            // this is the moment it is least true.
            pending.note_server_spoke();
            for (uri, version) in documents {
                pending.mark(lifecycle_id, &uri, version, now);
            }
        }
    }

    fn pending_file_count(&self) -> usize {
        self.pending_diagnostics_by_server
            .values()
            .map(PendingEdits::len)
            .sum()
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending_file_count()
    }

    #[cfg(test)]
    pub fn is_uri_pending(&self, path: &Path) -> bool {
        file_uri(path)
            .map(|uri| {
                self.pending_diagnostics_by_server
                    .values()
                    .any(|pending| pending.contains(uri.as_str()))
            })
            .unwrap_or(false)
    }

    /// Take the verdicts the servers have given on the files we are waiting on,
    /// and report the problems among them.
    ///
    /// Every file this settles leaves the pending set, whether or not it
    /// produced a line to show: "no problems" is a verdict, and a file that
    /// keeps waiting for one it has already had is what makes the set grow
    /// without bound.
    fn take_answered_diagnostics(&mut self) -> Option<DiagnosticsSummary> {
        let now = Instant::now();
        let mut collected = CollectedDiagnostics::default();

        // In server-name order, so what the reader sees does not depend on how
        // a hash map happened to lay itself out.
        let mut servers: Vec<&String> = self.pending_diagnostics_by_server.keys().collect();
        servers.sort_unstable();
        let servers: Vec<String> = servers.into_iter().cloned().collect();

        for server_name in servers {
            let Some(pending) = self.pending_diagnostics_by_server.get_mut(&server_name) else {
                continue;
            };
            // The questions were put to a server that is gone, or that has been
            // replaced. Either way nobody is going to answer them.
            let Some(client) = self.clients.get(&server_name) else {
                pending.clear();
                continue;
            };
            if client.lifecycle_id != pending.lifecycle_id() {
                pending.clear();
                continue;
            }

            for uri in pending.take_answered(&server_name, &client.diagnostics, now) {
                collected.append_file(&uri, client.diagnostics.items(&uri));
            }
        }

        if collected.lines.is_empty() {
            return None;
        }
        if let Some(note) = collected.trimmed_note() {
            collected.lines.push(note);
        }

        Some(DiagnosticsSummary {
            text: format!(
                "<lsp-diagnostics>\n{}\n</lsp-diagnostics>",
                collected.lines.join("\n")
            ),
            file_count: collected.file_count,
            diagnostic_count: collected.diagnostic_count,
        })
    }

    /// Auto-open file if needed, return cloned socket for lock-free dispatch.
    /// Single resolve — no double lookup.
    pub async fn socket_for_file(&mut self, path: &Path) -> Option<async_lsp::ServerSocket> {
        let (server_name, lang_id) = super::config::resolve_server(&self.servers, path)?;
        // Auto-open if not yet tracked.
        let needs_open = self
            .clients
            .get(&server_name)
            .and_then(|c| {
                file_uri(path)
                    .ok()
                    .map(|uri| !c.documents.contains(uri.as_str()))
            })
            .unwrap_or(false);
        if needs_open
            && let Ok(content) = tokio::fs::read_to_string(path).await
            && let Some(client) = self.clients.get_mut(&server_name)
        {
            client.notify_file_change(path, &content, &lang_id);
            tokio::task::yield_now().await;
        }
        self.clients.get(&server_name).map(|c| c.socket.clone())
    }

    pub fn all_sockets(&self) -> Vec<async_lsp::ServerSocket> {
        self.clients.values().map(|c| c.socket.clone()).collect()
    }

    #[cfg(test)]
    pub fn client_for_file_mut(&mut self, path: &Path) -> Option<&mut LspClient> {
        let (server_name, _) = super::config::resolve_server(&self.servers, path)?;
        self.clients.get_mut(&server_name)
    }

    #[cfg(test)]
    pub async fn dispatch_tool_typed(
        &mut self,
        input: &super::LspToolInput,
    ) -> super::LspToolResult {
        use super::{LspOperation, LspToolResult};

        let err = |msg: String| LspToolResult {
            text: msg,
            is_error: true,
        };

        let result = match input.operation {
            LspOperation::GoToDefinition
            | LspOperation::FindReferences
            | LspOperation::Hover
            | LspOperation::GoToImplementation => {
                let (Some(fp), Some(line), Some(col)) =
                    (&input.file_path, input.line, input.character)
                else {
                    return err("Required: file_path (string), line (int), character (int).".into());
                };
                let path = PathBuf::from(fp);
                let Some(client) = self.client_for_file_mut(&path) else {
                    return err(format!("No LSP server configured for {}", path.display()));
                };
                match input.operation {
                    LspOperation::GoToDefinition => client
                        .goto_definition(&path, line, col)
                        .await
                        .map(|l| format_locations_labeled("Definition", &l)),
                    LspOperation::FindReferences => client
                        .goto_references(&path, line, col)
                        .await
                        .map(|l| format_locations_labeled("References", &l)),
                    LspOperation::GoToImplementation => client
                        .goto_implementation(&path, line, col)
                        .await
                        .map(|l| format_locations_labeled("Implementations", &l)),
                    LspOperation::Hover => client.hover(&path, line, col).await.map(|opt| {
                        opt.unwrap_or_else(|| "No hover information available.".to_string())
                    }),
                    _ => unreachable!(),
                }
            }
            LspOperation::DocumentSymbol => {
                let Some(ref file_path) = input.file_path else {
                    return err("Required: file_path (string).".into());
                };
                let path = PathBuf::from(file_path);
                let Some(client) = self.client_for_file_mut(&path) else {
                    return err(format!("No LSP server configured for {}", path.display()));
                };
                client
                    .document_symbols(&path)
                    .await
                    .map(|s| format_symbols(&s))
            }
            LspOperation::WorkspaceSymbol => {
                let Some(ref query) = input.query else {
                    return err("Required: query (string).".into());
                };
                if self.clients.is_empty() {
                    return err("No LSP servers are running.".into());
                }
                let mut all_symbols = Vec::new();
                let mut last_err = None;
                for client in self.clients.values_mut() {
                    match client.workspace_symbols(query).await {
                        Ok(symbols) => all_symbols.extend(symbols),
                        Err(e) => {
                            last_err = Some(e);
                        }
                    }
                }
                if all_symbols.is_empty() {
                    match last_err {
                        Some(e) => Err(e),
                        None => Ok(format_symbols(&[])),
                    }
                } else {
                    Ok(format_symbols(&all_symbols))
                }
            }
        };

        match result {
            Ok(text) => LspToolResult {
                text,
                is_error: false,
            },
            Err(e) => {
                tracing::warn!(error = %e, "LSP tool failed");
                LspToolResult {
                    text: format!("LSP error: {e}"),
                    is_error: true,
                }
            }
        }
    }
}

/// Wait, up to `timeout`, for the servers to say something about the files we
/// have told them about, and report whatever they said.
///
/// Drops the lock across the wait so `notify_file_changed` isn't blocked.
pub async fn drain_lsp_diagnostics(
    lsp_manager: &tokio::sync::Mutex<LspManager>,
    timeout: std::time::Duration,
) -> Option<DiagnosticsSummary> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut lsp = lsp_manager.lock().await;
    // Set once the budget is spent. Checked only *after* collecting, so the
    // store is always read one final time before we conclude there was nothing:
    // a report can land between the wait's last poll and our re-taking the
    // lock, and it would otherwise sit unread.
    let mut out_of_time = false;

    loop {
        // A refresh can land at any point, including during the wait below.
        lsp.reopen_refreshed_questions();

        if !lsp.has_pending_diagnostics() {
            return None;
        }

        // The answer may already be in; on the first pass that saves the wait
        // entirely, and on later passes it is what the wait was for.
        if let Some(summary) = lsp.take_answered_diagnostics() {
            return Some(summary);
        }

        // Nothing to report, and either the budget is gone or every server
        // still owing us a verdict has stopped talking. Both mean stop.
        if out_of_time || !lsp.worth_blocking_for_diagnostics() {
            tracing::debug!(
                pending_file_count = lsp.pending_file_count(),
                timeout_ms = timeout.as_millis() as u64,
                timed_out = out_of_time,
                "no LSP diagnostics for pending files"
            );
            return None;
        }

        // Register the waiter before dropping the lock so a notify_one() that
        // lands in between is not lost.
        let notify = lsp.diagnostics_ready.clone();
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        drop(lsp);

        // Every document shares this notification, so being woken is not proof
        // that *our* files were answered — a publish for some other file wakes
        // us just the same. Go back and look, and if it was not for us, keep
        // waiting until it is or the budget runs out.
        out_of_time = tokio::time::timeout_at(deadline, notified).await.is_err();
        lsp = lsp_manager.lock().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_lsp::lsp_types::{Position, Range};

    fn diagnostic(line: u32, severity: DiagnosticSeverity, message: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 1 },
            },
            severity: Some(severity),
            message: message.to_string(),
            ..Default::default()
        }
    }

    fn errors(n: u32) -> Vec<Diagnostic> {
        (0..n)
            .map(|i| diagnostic(i, DiagnosticSeverity::ERROR, &format!("error {i}")))
            .collect()
    }

    /// A file with forty problems is usually one mistake seen forty times, and
    /// the reader is worse off for having all of them.
    #[test]
    fn one_file_cannot_fill_the_whole_summary() {
        let mut collected = CollectedDiagnostics::default();
        collected.append_file("file:///a.cs", errors(40));

        assert_eq!(collected.lines.len(), MAX_PER_FILE + 1, "plus the header");
        assert_eq!(
            collected.diagnostic_count, 40,
            "the count is what the server said, not what survived the trim"
        );
        assert_eq!(
            collected.trimmed_note().as_deref(),
            Some("… and 30 more not shown"),
            "silently truncating would let the reader take a partial list for the whole truth"
        );
    }

    /// A refresh re-opens every open document at once, so the ceiling has to
    /// hold across files and not just within one.
    #[test]
    fn a_refresh_over_many_files_cannot_flood_one_turn() {
        let mut collected = CollectedDiagnostics::default();
        for i in 0..20 {
            collected.append_file(&format!("file:///f{i}.cs"), errors(5));
        }

        let shown = collected
            .lines
            .iter()
            .filter(|l| l.starts_with("  "))
            .count();
        assert_eq!(shown, MAX_PER_SUMMARY);
        assert_eq!(collected.diagnostic_count, 100);
        assert_eq!(
            collected.trimmed_note().as_deref(),
            Some("… and 70 more not shown")
        );
    }

    /// When something has to go, warnings go first.
    #[test]
    fn errors_come_before_warnings() {
        let mut collected = CollectedDiagnostics::default();
        let mut items = vec![
            diagnostic(9, DiagnosticSeverity::WARNING, "a warning"),
            diagnostic(1, DiagnosticSeverity::ERROR, "an error"),
        ];
        items.extend(
            (0..MAX_PER_FILE as u32)
                .map(|i| diagnostic(20 + i, DiagnosticSeverity::WARNING, "filler")),
        );
        collected.append_file("file:///a.cs", items);

        assert!(
            collected.lines[1].contains("an error"),
            "{:?}",
            collected.lines
        );
        assert!(
            collected.lines[2].contains("a warning"),
            "{:?}",
            collected.lines
        );
    }

    #[test]
    fn nothing_worth_showing_adds_no_header() {
        let mut collected = CollectedDiagnostics::default();
        collected.append_file(
            "file:///a.cs",
            vec![diagnostic(0, DiagnosticSeverity::HINT, "just a hint")],
        );
        assert!(collected.lines.is_empty());
        assert_eq!(collected.file_count, 0);
        assert_eq!(collected.trimmed_note(), None);
    }

    #[test]
    fn a_summary_that_fits_says_nothing_about_trimming() {
        let mut collected = CollectedDiagnostics::default();
        collected.append_file("file:///a.cs", errors(3));
        assert_eq!(collected.trimmed_note(), None);
        assert_eq!(collected.file_count, 1);
    }
}
