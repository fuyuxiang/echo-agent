//! `/export [filename]` -- export the current conversation transcript as Markdown.
//!
//! Omit the filename (or pass empty) to copy the full transcript to the clipboard.
//! With a filename, writes a UTF-8 .md file (supports ~ expansion, paths with spaces,
//! and parent directory creation).
//!
//! Pager-side only (local TUI execution). Follows the exact patterns from
//! `copy.rs`, `share.rs`, and the SlashCommand trait in `command.rs`.

use std::path::{Path, PathBuf};

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

/// Export the current conversation to a file or clipboard.
pub struct ExportCommand;

impl SlashCommand for ExportCommand {
    fn name(&self) -> &str {
        "export"
    }

    fn description(&self) -> &str {
        "Export the current conversation to a file or clipboard"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/export [filename]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("[filename]")
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let items = list_path_completions(ctx.cwd, args_query);
        if items.is_empty() { None } else { Some(items) }
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session to export".to_string());
        }

        let trimmed = args.trim();
        let file_path: Option<PathBuf> = if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        };

        CommandResult::Action(Action::ExportConversation { file_path })
    }
}

/// List filesystem entries for path completion in the `/export` args dropdown.
///
/// Parses the typed query to extract a directory prefix, lists its contents,
/// and returns `ArgItem`s. Directories get a trailing `/` in `insert_text` so
/// the dropdown stays open for drill-down (same trick `/model` uses with
/// trailing space for effort chaining).
///
/// The `SlashController` handles nucleo fuzzy ranking on the returned items
/// automatically — we just provide the candidates.
///
/// Synchronous `read_dir` — same pattern as `/model` and `/theme` which query
/// `ModelState` synchronously. Local directory listing is sub-millisecond;
/// the 1000-entry pre-sort cap guards against pathological directories.
/// Moving to the `@`-style background daemon would require adding tick-based
/// polling to the slash command system (which is currently event-driven only).
fn list_path_completions(cwd: &Path, query: &str) -> Vec<ArgItem> {
    let trimmed = query.trim_start();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let input_path = PathBuf::from(shellexpand::tilde(trimmed).as_ref());

    // Determine which directory to list and what prefix the user has typed.
    // If the input ends with `/`, list that directory's contents.
    // Otherwise, list the parent and let nucleo filter by the partial filename.
    let (dir_to_list, typed_prefix) = if trimmed.ends_with('/') {
        (input_path.clone(), trimmed.to_string())
    } else {
        let parent = input_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(cwd);
        // Reconstruct the user's prefix up to the last `/` (preserving ~).
        let prefix = match trimmed.rfind('/') {
            Some(pos) => &trimmed[..=pos],
            None => "",
        };
        (parent.to_path_buf(), prefix.to_string())
    };

    // Resolve relative paths against cwd.
    let resolved = if dir_to_list.is_relative() {
        cwd.join(&dir_to_list)
    } else {
        dir_to_list
    };

    let entries = match std::fs::read_dir(&resolved) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut items: Vec<ArgItem> = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with('.') {
            continue;
        }

        // Follow symlinks so symlinked directories get trailing `/`.
        let is_dir = entry.path().is_dir();
        let suffix = if is_dir { "/" } else { "" };

        items.push(ArgItem {
            display: format!("{name_str}{suffix}"),
            match_text: format!("{typed_prefix}{name_str}"),
            insert_text: format!("{typed_prefix}{name_str}{suffix}"),
            description: if is_dir {
                "directory".to_string()
            } else {
                "file".to_string()
            },
        });

        // Pre-sort cap to avoid pathological directories.
        if items.len() >= 1000 {
            break;
        }
    }

    // Sort: directories first, then alphabetical. Truncate after sort.
    items.sort_by(|a, b| {
        let a_dir = a.display.ends_with('/');
        let b_dir = b.display.ends_with('/');
        b_dir.cmp(&a_dir).then_with(|| a.display.cmp(&b.display))
    });
    items.truncate(100);

    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::actions::Action;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    static DEFAULT_BUNDLE_STATE: BundleState = BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    fn make_ctx(models: &ModelState) -> CommandExecCtx<'_> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn no_session_errors() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = ExportCommand;
        match cmd.run(&mut ctx, "") {
            CommandResult::Error(msg) => assert!(msg.contains("No active session")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn dispatches_clipboard_when_no_path() {
        let models = ModelState::default();
        let sid = agent_client_protocol::SessionId::from("test-session".to_string());
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: Some(&sid),
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        let cmd = ExportCommand;
        match cmd.run(&mut ctx, "   ") {
            CommandResult::Action(Action::ExportConversation { file_path }) => {
                assert!(file_path.is_none());
            }
            other => panic!("expected ExportConversation(None), got {other:?}"),
        }
    }

    #[test]
    fn dispatches_file_path_when_given() {
        let models = ModelState::default();
        let sid = agent_client_protocol::SessionId::from("s2".to_string());
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: Some(&sid),
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        let cmd = ExportCommand;
        match cmd.run(&mut ctx, "~/exports/my convo with spaces.md") {
            CommandResult::Action(Action::ExportConversation { file_path }) => {
                let p = file_path.expect("some path");
                assert!(p.to_string_lossy().contains("my convo with spaces.md"));
            }
            other => panic!("expected ExportConversation(Some), got {other:?}"),
        }
    }
}
