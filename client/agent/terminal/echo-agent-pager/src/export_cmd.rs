use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::acp::meta::NotificationMeta;
use crate::acp::tracker::AcpUpdateTracker;
use crate::scrollback::export::render_blocks_to_markdown;
use crate::scrollback::state::ScrollbackState;

#[derive(Debug, clap::Args, Clone)]
pub struct ExportArgs {
    /// Session ID to export
    pub session_id: String,
    /// Output file path (default: stdout)
    pub output: Option<PathBuf>,
    /// Copy to clipboard instead of writing to stdout
    #[arg(long, short)]
    pub clipboard: bool,
}

pub fn run(args: ExportArgs) -> Result<()> {
    tracing::info!(session_id = %args.session_id, "export_cmd: starting session export");

    let updates = echo_agent_runtime::session::storage::load_updates_for_replay(&args.session_id)?
        .with_context(|| format!("Session '{}' not found.", args.session_id))?;

    let mut tracker = AcpUpdateTracker::new();
    let mut scrollback = ScrollbackState::new();
    let replay_meta = NotificationMeta {
        is_replay: true,
        ..Default::default()
    };

    for update in updates {
        tracker.handle_update(update, &replay_meta, &mut scrollback);
    }

    let blocks: Vec<_> = (0..scrollback.len())
        .filter_map(|i| scrollback.entry(i).map(|e| &e.block))
        .collect();
    let md = render_blocks_to_markdown(blocks);

    if md.is_empty() {
        anyhow::bail!(
            "Session '{}' has no conversation content to export",
            args.session_id
        );
    }

    if let Some(path) = args.output {
        let expanded = PathBuf::from(shellexpand::tilde(&path.to_string_lossy()).as_ref());
        if let Some(parent) = expanded.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        std::fs::write(&expanded, &md)
            .with_context(|| format!("Failed to write {}", expanded.display()))?;
        tracing::info!(
            session_id = %args.session_id,
            path = %expanded.display(),
            bytes = md.len(),
            "export_cmd: wrote transcript to file"
        );
        eprintln!("Conversation exported to {}", expanded.display());
    } else if args.clipboard {
        let _ = crate::clipboard::copy_text(&md);
        let lines = md.lines().count();
        tracing::info!(
            session_id = %args.session_id,
            bytes = md.len(),
            lines,
            "export_cmd: copied transcript to clipboard"
        );
        eprintln!(
            "Conversation copied to clipboard ({} chars, {} lines)",
            md.len(),
            lines
        );
    } else {
        std::io::stdout().write_all(md.as_bytes())?;
        std::io::stdout().write_all(b"\n")?;
    }

    Ok(())
}
