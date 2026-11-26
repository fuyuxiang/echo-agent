//! Cross-cutting reminder: notifies LSP of file changes and drains diagnostics.

use std::sync::Arc;

use crate::implementations::lsp::LspBackend;
use crate::types::output::{SearchReplaceOutput, ToolOutput};
use crate::types::resources::SharedResources;
use crate::types::tool::Reminder;

pub struct LspDiagnosticsReminder;

#[async_trait::async_trait]
impl Reminder for LspDiagnosticsReminder {
    async fn collect_reminders(
        &self,
        resources: SharedResources,
        tool_output: &ToolOutput,
    ) -> Vec<String> {
        let lsp = {
            let res = resources.lock().await;
            match res.get::<Arc<dyn LspBackend>>() {
                Some(h) => h.clone(),
                None => return vec![],
            }
        };

        lsp.ensure_started_background();

        // After SearchReplace edits, notify LSP so diagnostics refresh.
        // The adapter routes immediately when ready and buffers pre-ready edits otherwise.
        if let ToolOutput::SearchReplace(SearchReplaceOutput::EditsApplied(edits)) = tool_output
            && let Ok(content) = std::fs::read_to_string(&edits.absolute_path)
        {
            lsp.notify_file_changed(&edits.absolute_path, &content)
                .await;
        }

        // Drain any pending diagnostics (from this or previous edits).
        if let Some(summary) = lsp
            .drain_diagnostics(crate::implementations::lsp::DIAGNOSTICS_DRAIN_TIMEOUT)
            .await
        {
            return vec![summary.text];
        }

        vec![]
    }
}
