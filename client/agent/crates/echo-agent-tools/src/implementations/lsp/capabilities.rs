//! What a server told us it wants during `initialize`, and the document
//! bookkeeping that follows from it.
//!
//! EchoAgent used to log the initialize result and throw it away, which is how it
//! ended up sending Roslyn a change event the protocol says must carry a range.
//! Everything the handshake tells us that changes what we send lives here.

use async_lsp::lsp_types::{
    DidSaveTextDocumentParams, Position, Range, ServerCapabilities, TextDocumentIdentifier,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncSaveOptions, Url,
};

/// What the server asked us to do on save.
///
/// A server may want no `didSave` at all, or one without the document text.
/// Sending the full text unconditionally violates the protocol and ships a copy
/// of the file on every edit to a server that will discard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavePolicy {
    /// The server did not ask to be told about saves.
    Skip,
    /// Notify, but without the document text.
    WithoutText,
    /// Notify and include the full document text.
    WithText,
}

/// The parts of a server's advertised capabilities that change what we send it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPolicy {
    /// The server declared incremental text sync. Such servers reject a change
    /// event without a range — Roslyn dereferences it and tears its request
    /// queue down — so a whole-document range has to be supplied instead.
    pub sync_incremental: bool,
    pub save: SavePolicy,
    /// The server advertised a `textDocument/diagnostic` provider. Absence is
    /// not proof of absence; see `pull::PullSupport`.
    pub advertises_pull: bool,
}

impl ServerPolicy {
    pub fn from_capabilities(capabilities: &ServerCapabilities) -> Self {
        let sync = capabilities.text_document_sync.as_ref();
        Self {
            sync_incremental: wants_incremental_sync(sync),
            save: save_policy(sync),
            advertises_pull: capabilities.diagnostic_provider.is_some(),
        }
    }

    /// The range to attach to a change event that replaces the whole document,
    /// given where the previous revision ended.
    ///
    /// We always resend the whole file. A server that asked for incremental
    /// sync still requires a range on every change event, so the full
    /// replacement is expressed as a range covering the previous revision.
    /// Full-sync servers get the rangeless form they expect.
    pub fn full_replacement_range(&self, previous_end: Position) -> Option<Range> {
        self.sync_incremental.then_some(Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: previous_end,
        })
    }

    /// The `didSave` to send for this document, or `None` if the server did not
    /// ask to hear about saves at all.
    pub fn did_save(&self, uri: Url, content: &str) -> Option<DidSaveTextDocumentParams> {
        let text = match self.save {
            SavePolicy::Skip => return None,
            SavePolicy::WithoutText => None,
            SavePolicy::WithText => Some(content.to_string()),
        };
        Some(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text,
        })
    }
}

/// Whether the server asked for incremental text synchronization.
///
/// Servers advertise this either as a bare kind or inside sync options; both
/// spellings mean the same thing for our purposes. Anything else (including an
/// absent capability) is treated as full-document sync, which is what we send.
fn wants_incremental_sync(cap: Option<&TextDocumentSyncCapability>) -> bool {
    let kind = match cap {
        Some(TextDocumentSyncCapability::Kind(kind)) => Some(*kind),
        Some(TextDocumentSyncCapability::Options(opts)) => opts.change,
        None => None,
    };
    kind == Some(TextDocumentSyncKind::INCREMENTAL)
}

/// What the server asked for on save, from its sync capability.
fn save_policy(cap: Option<&TextDocumentSyncCapability>) -> SavePolicy {
    match cap {
        // A bare sync kind says nothing about save. Notify without the text,
        // which is what other clients do and is enough for servers that only
        // recompute on save.
        Some(TextDocumentSyncCapability::Kind(_)) => SavePolicy::WithoutText,
        Some(TextDocumentSyncCapability::Options(opts)) => match opts.save.as_ref() {
            // Save omitted entirely (Roslyn) or explicitly declined.
            None | Some(TextDocumentSyncSaveOptions::Supported(false)) => SavePolicy::Skip,
            Some(TextDocumentSyncSaveOptions::Supported(true)) => SavePolicy::WithoutText,
            Some(TextDocumentSyncSaveOptions::SaveOptions(options)) => {
                if options.include_text.unwrap_or(false) {
                    SavePolicy::WithText
                } else {
                    SavePolicy::WithoutText
                }
            }
        },
        // The server declared no sync capability at all, so we cannot tell.
        // Keep the historical behaviour rather than risk silencing a server
        // that only reports diagnostics after a save.
        None => SavePolicy::WithText,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_lsp::lsp_types::{DiagnosticOptions, DiagnosticServerCapabilities, SaveOptions};

    fn uri() -> Url {
        Url::parse("file:///a.cs").unwrap()
    }

    fn position(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    fn policy(capabilities: ServerCapabilities) -> ServerPolicy {
        ServerPolicy::from_capabilities(&capabilities)
    }

    #[test]
    fn a_bare_incremental_kind_means_incremental() {
        let p = policy(ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(
                TextDocumentSyncKind::INCREMENTAL,
            )),
            ..Default::default()
        });
        assert!(p.sync_incremental);
        assert_eq!(p.save, SavePolicy::WithoutText);
    }

    #[test]
    fn roslyns_shape_is_incremental_with_no_save() {
        // {"openClose": true, "change": 2} — no `save` key at all.
        let p = policy(ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                async_lsp::lsp_types::TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(TextDocumentSyncKind::INCREMENTAL),
                    ..Default::default()
                },
            )),
            diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                DiagnosticOptions::default(),
            )),
            ..Default::default()
        });
        assert!(p.sync_incremental);
        assert_eq!(p.save, SavePolicy::Skip);
        assert!(p.advertises_pull);
        assert!(p.did_save(uri(), "body").is_none());
    }

    #[test]
    fn include_text_is_the_only_way_to_get_the_text() {
        let with_text = policy(ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                async_lsp::lsp_types::TextDocumentSyncOptions {
                    change: Some(TextDocumentSyncKind::FULL),
                    save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                        include_text: Some(true),
                    })),
                    ..Default::default()
                },
            )),
            ..Default::default()
        });
        assert_eq!(with_text.save, SavePolicy::WithText);
        assert_eq!(
            with_text.did_save(uri(), "body").and_then(|p| p.text),
            Some("body".to_string())
        );

        let without_text = policy(ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                async_lsp::lsp_types::TextDocumentSyncOptions {
                    save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                    ..Default::default()
                },
            )),
            ..Default::default()
        });
        assert_eq!(without_text.save, SavePolicy::WithoutText);
        let sent = without_text
            .did_save(uri(), "body")
            .expect("the server asked to hear about saves");
        assert_eq!(sent.text, None, "but not to be sent the file with them");
    }

    #[test]
    fn a_server_with_no_sync_capability_keeps_the_historical_behaviour() {
        let p = policy(ServerCapabilities::default());
        assert!(!p.sync_incremental);
        assert_eq!(p.save, SavePolicy::WithText);
        assert!(!p.advertises_pull);
    }

    #[test]
    fn only_incremental_servers_get_a_range() {
        let incremental = ServerPolicy {
            sync_incremental: true,
            save: SavePolicy::Skip,
            advertises_pull: false,
        };
        assert_eq!(
            incremental.full_replacement_range(position(1, 5)),
            Some(Range {
                start: position(0, 0),
                end: position(1, 5),
            })
        );

        let full = ServerPolicy {
            sync_incremental: false,
            ..incremental
        };
        assert_eq!(full.full_replacement_range(position(1, 5)), None);
    }
}
