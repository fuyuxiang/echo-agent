//! External-editor request and prompt-draft lifecycle.
//!
//! Prompt files are materialized at TTY handoff and removed by `Drop`.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::app::agent::AgentId;
use crate::app::app_view::{ActiveView, AppView};
use crate::scrollback::block::RenderBlock;

const PROMPT_EDITOR_MAX_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const ATTACHMENT_MESSAGE: &str =
    "External prompt editing is not available while the draft has attachments.";
pub(crate) const VOICE_MESSAGE: &str =
    "External prompt editing is not available while voice input is active.";
pub(crate) const PASTE_MESSAGE: &str =
    "External prompt editing is not available while a paste is still being processed.";
pub(crate) const OWNERSHIP_MESSAGE: &str =
    "External prompt editing was cancelled because another input surface became active.";
const PROMPT_EDITOR_PREPARE_FAILURE: &str =
    "Could not open the draft in an external editor; the original draft was kept.";
const PROMPT_EDITOR_FAILURE: &str = "External prompt editor failed; the original draft was kept.";
const PROMPT_EDITOR_NONZERO: &str =
    "External prompt editor exited unsuccessfully; the original draft was kept.";
const PROMPT_EDITOR_INVALID_UTF8: &str =
    "External prompt editor saved invalid UTF-8; the original draft was kept.";
const PROMPT_EDITOR_TOO_LARGE: &str =
    "External prompt editor saved a draft larger than 4 MiB; the original draft was kept.";
const PROMPT_EDITOR_STALE: &str =
    "The draft changed while the external editor was open; the newer draft was kept.";

#[derive(Clone, Debug)]
pub(crate) enum PendingEditorRequest {
    /// Edit an agents/personas configuration file, then refresh its modal tab.
    ConfigFile {
        path: PathBuf,
        refresh_agents_modal: Option<crate::views::agents_modal::AgentsTab>,
    },
    /// Edit an attachment-free composer draft.
    PromptDraft {
        agent_id: AgentId,
        original_text: String,
    },
}

pub(crate) struct EditorLaunch {
    pub(crate) argv: Vec<String>,
    pub(crate) path: PathBuf,
}

pub(crate) enum PreparedEditorRequest {
    ConfigFile {
        launch: EditorLaunch,
        refresh_agents_modal: Option<crate::views::agents_modal::AgentsTab>,
    },
    PromptDraft {
        launch: EditorLaunch,
        agent_id: AgentId,
        original_text: String,
        file: PromptEditorFile,
    },
}

impl PreparedEditorRequest {
    pub(crate) fn launch(&self) -> &EditorLaunch {
        match self {
            Self::ConfigFile { launch, .. } | Self::PromptDraft { launch, .. } => launch,
        }
    }
}

pub(crate) struct PromptEditorFile {
    path: PathBuf,
}

impl PromptEditorFile {
    fn create(text: &str) -> std::io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("echo-agent-prompt-{}.md", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let owner = Self { path };
        file.write_all(text.as_bytes())?;
        file.flush()?;
        Ok(owner)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<String, &'static str> {
        let file = std::fs::File::open(&self.path).map_err(|_| PROMPT_EDITOR_FAILURE)?;
        if file.metadata().map_err(|_| PROMPT_EDITOR_FAILURE)?.len() > PROMPT_EDITOR_MAX_BYTES {
            return Err(PROMPT_EDITOR_TOO_LARGE);
        }
        let mut bytes = Vec::new();
        file.take(PROMPT_EDITOR_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PROMPT_EDITOR_FAILURE)?;
        if bytes.len() as u64 > PROMPT_EDITOR_MAX_BYTES {
            return Err(PROMPT_EDITOR_TOO_LARGE);
        }
        String::from_utf8(bytes).map_err(|_| PROMPT_EDITOR_INVALID_UTF8)
    }
}

impl Drop for PromptEditorFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, path = %self.path.display(), "external prompt editor: cleanup failed");
        }
    }
}

fn parse_editor_argv(command: &str) -> Result<Vec<String>, String> {
    shlex::split(command)
        .filter(|parts| parts.first().is_some_and(|program| !program.is_empty()))
        .ok_or_else(|| "could not parse $VISUAL or $EDITOR".to_owned())
}

fn resolve_editor_argv(visual: Option<&str>, editor: Option<&str>) -> Result<Vec<String>, String> {
    let command = visual
        .filter(|value| !value.trim().is_empty())
        .or_else(|| editor.filter(|value| !value.trim().is_empty()))
        .unwrap_or("vi");
    parse_editor_argv(command)
}

fn editor_argv() -> Result<Vec<String>, String> {
    let visual = std::env::var("VISUAL").ok();
    let editor = std::env::var("EDITOR").ok();
    resolve_editor_argv(visual.as_deref(), editor.as_deref())
}

/// Revalidate immediately before creating a prompt file.
fn revalidate(app: &mut AppView, request: PendingEditorRequest) -> Option<PendingEditorRequest> {
    let agent_id = match &request {
        PendingEditorRequest::PromptDraft { agent_id, .. } => *agent_id,
        PendingEditorRequest::ConfigFile { .. } => return Some(request),
    };
    let access = app
        .agents
        .get(&agent_id)
        .map(|agent| agent.external_prompt_editor_access());
    let message = if app.voice_recording_target()
        == Some(crate::app::app_view::VoiceTarget::Agent(agent_id))
    {
        Some(VOICE_MESSAGE)
    } else {
        match access {
            Some(crate::app::agent_view::ExternalPromptEditorAccess::Ready) => None,
            Some(crate::app::agent_view::ExternalPromptEditorAccess::Attachments) => {
                Some(ATTACHMENT_MESSAGE)
            }
            Some(crate::app::agent_view::ExternalPromptEditorAccess::PastePending) => {
                Some(PASTE_MESSAGE)
            }
            Some(crate::app::agent_view::ExternalPromptEditorAccess::OwnedElsewhere) => {
                Some(OWNERSHIP_MESSAGE)
            }
            None => return None,
        }
    };
    if let Some(message) = message {
        report_prompt_failure(app, agent_id, message);
        None
    } else {
        Some(request)
    }
}

pub(crate) fn prepare(
    app: &mut AppView,
    request: PendingEditorRequest,
) -> Result<Option<PreparedEditorRequest>, PrepareError> {
    let Some(request) = revalidate(app, request) else {
        return Ok(None);
    };
    let argv = match editor_argv() {
        Ok(argv) => argv,
        Err(message) => {
            return Err(PrepareError { request, message });
        }
    };
    match request {
        PendingEditorRequest::ConfigFile {
            path,
            refresh_agents_modal,
        } => Ok(Some(PreparedEditorRequest::ConfigFile {
            launch: EditorLaunch { argv, path },
            refresh_agents_modal,
        })),
        PendingEditorRequest::PromptDraft {
            agent_id,
            original_text,
        } => match PromptEditorFile::create(&original_text) {
            Ok(file) => Ok(Some(PreparedEditorRequest::PromptDraft {
                launch: EditorLaunch {
                    argv,
                    path: file.path().to_path_buf(),
                },
                agent_id,
                original_text,
                file,
            })),
            Err(error) => {
                tracing::warn!(%error, "external prompt editor: temp-file creation failed");
                Err(PrepareError {
                    request: PendingEditorRequest::PromptDraft {
                        agent_id,
                        original_text,
                    },
                    message: PROMPT_EDITOR_PREPARE_FAILURE.to_owned(),
                })
            }
        },
    }
}

#[derive(Debug)]
pub(crate) struct PrepareError {
    request: PendingEditorRequest,
    message: String,
}

pub(crate) fn finish_prepare_error(app: &mut AppView, error: PrepareError) {
    tracing::warn!(error = %error.message, "external editor: could not prepare child");
    match error.request {
        PendingEditorRequest::PromptDraft { agent_id, .. } => {
            report_prompt_failure(app, agent_id, &error.message);
        }
        PendingEditorRequest::ConfigFile { .. } => report_config_failure(app, &error.message),
    }
}

fn report_config_failure(app: &mut AppView, message: &str) {
    if app.screen_mode.is_minimal()
        && let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
    {
        let block = RenderBlock::system(message.to_owned());
        if let Some(child_sid) = agent.active_subagent.clone()
            && let Some(child) = agent.subagent_views.get_mut(&child_sid)
        {
            child.scrollback.push_block(block);
        } else {
            agent.scrollback.push_block(block);
        }
    } else {
        app.show_toast(message);
    }
}

pub(crate) fn finish(
    app: &mut AppView,
    prepared: PreparedEditorRequest,
    editor_result: Result<std::process::ExitStatus, std::io::Error>,
) {
    match prepared {
        PreparedEditorRequest::ConfigFile {
            refresh_agents_modal,
            ..
        } => {
            if let Err(error) = editor_result {
                tracing::warn!(%error, "configuration editor: child failed");
            }
            if let Some(tab) = refresh_agents_modal
                && let ActiveView::Agent(id) = app.active_view
                && let Some(agent) = app.agents.get_mut(&id)
                && let Some(ref mut modal) = agent.agents_modal
            {
                modal.refresh_after_editor(tab);
            }
        }
        PreparedEditorRequest::PromptDraft {
            agent_id,
            original_text,
            file,
            ..
        } => {
            // Editors append a final newline on save only when the buffer
            // doesn't already end with one, so it is editor-added exactly
            // when the draft lacked one — strip that single newline.
            let strip_editor_newline = !original_text.ends_with('\n');
            let outcome = match editor_result {
                Ok(status) if status.success() => file.read().map(|mut text| {
                    if strip_editor_newline && text.ends_with('\n') {
                        text.pop();
                        if text.ends_with('\r') {
                            text.pop();
                        }
                    }
                    text
                }),
                Ok(_) => Err(PROMPT_EDITOR_NONZERO),
                Err(error) => {
                    tracing::warn!(%error, "external prompt editor: child failed");
                    Err(PROMPT_EDITOR_FAILURE)
                }
            };
            apply_prompt_outcome(app, agent_id, original_text, outcome);
        }
    }
}

fn apply_prompt_outcome(
    app: &mut AppView,
    agent_id: AgentId,
    original_text: String,
    outcome: Result<String, &'static str>,
) {
    if app
        .agents
        .get(&agent_id)
        .is_some_and(|agent| agent.prompt.text() != original_text)
    {
        tracing::warn!(
            agent_id = agent_id.0,
            "external prompt editor: draft changed while editor was open"
        );
        report_prompt_failure(app, agent_id, PROMPT_EDITOR_STALE);
        return;
    }
    match outcome {
        Ok(text) => apply_prompt_text(app, agent_id, text),
        Err(message) => report_prompt_failure(app, agent_id, message),
    }
}

pub(crate) fn apply_prompt_text(app: &mut AppView, agent_id: AgentId, text: String) {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return;
    };
    agent.prompt.history_search.deactivate();
    agent.prompt.set_text(&text);
    agent.prompt.clear_history();
    agent.prompt.set_cursor(text.len());
    agent.prompt.refresh_slash(&agent.session.models);
    agent.prompt.prompt_suggestion.clear();
}

pub(crate) fn report_prompt_failure(app: &mut AppView, agent_id: AgentId, message: &str) {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent
            .scrollback
            .push_block(RenderBlock::system(message.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success_status() -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        }
    }

    #[test]
    fn editor_resolution_and_parsing_follow_visual_editor_vi_order() {
        assert_eq!(
            resolve_editor_argv(Some("visual --wait"), Some("editor")).unwrap(),
            ["visual", "--wait"]
        );
        assert_eq!(
            resolve_editor_argv(Some("  "), Some("editor --name 'prompt draft'")).unwrap(),
            ["editor", "--name", "prompt draft"]
        );
        assert_eq!(resolve_editor_argv(None, None).unwrap(), ["vi"]);
        assert!(parse_editor_argv("editor 'unterminated").is_err());
        assert!(parse_editor_argv("   ").is_err());
    }

    #[test]
    fn prompt_file_preserves_text_rejects_bad_input_and_cleans_on_drop() {
        let file = PromptEditorFile::create("first\nsecond\n").unwrap();
        let path = file.path().to_path_buf();
        assert_eq!(file.read().unwrap(), "first\nsecond\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(file);
        assert!(!path.exists());

        let file = PromptEditorFile::create("").unwrap();
        std::fs::write(file.path(), [0xff, 0xfe]).unwrap();
        assert_eq!(file.read(), Err(PROMPT_EDITOR_INVALID_UTF8));

        let file = PromptEditorFile::create("").unwrap();
        std::fs::File::options()
            .write(true)
            .open(file.path())
            .unwrap()
            .set_len(PROMPT_EDITOR_MAX_BYTES + 1)
            .unwrap();
        assert_eq!(file.read(), Err(PROMPT_EDITOR_TOO_LARGE));
    }

    #[test]
    fn prepared_prompt_file_cleans_up_when_never_spawned() {
        let file = PromptEditorFile::create("cancelled draft").expect("prepare prompt file");
        let prepared = PreparedEditorRequest::PromptDraft {
            launch: EditorLaunch {
                argv: vec!["editor".to_owned()],
                path: file.path().to_path_buf(),
            },
            agent_id: AgentId(9),
            original_text: "cancelled draft".to_owned(),
            file,
        };
        let path = prepared.launch().path.clone();
        assert!(path.exists());
        drop(prepared);
        assert!(!path.exists());
    }

    fn app_with_prompt_request() -> (AppView, PendingEditorRequest) {
        let id = AgentId(0);
        let mut app = crate::app::app_view::tests::test_app();
        let mut agent = crate::test_util::make_agent_view(Some("session"), "/work");
        agent.session.id = id;
        agent.prompt.set_text("original");
        app.agents.insert(id, agent);
        app.active_view = ActiveView::Agent(id);
        app.screen_mode = crate::app::ScreenMode::Minimal;
        (
            app,
            PendingEditorRequest::PromptDraft {
                agent_id: id,
                original_text: "original".to_owned(),
            },
        )
    }

    #[test]
    fn config_prepare_error_is_visible_in_fullscreen_and_minimal() {
        let id = AgentId(0);
        let mut app = crate::app::app_view::tests::test_app();
        app.agents.insert(
            id,
            crate::test_util::make_agent_view(Some("session"), "/work"),
        );
        app.active_view = ActiveView::Agent(id);
        let message = "could not parse $VISUAL or $EDITOR";
        let error = || PrepareError {
            request: PendingEditorRequest::ConfigFile {
                path: PathBuf::from("/tmp/agent-config.md"),
                refresh_agents_modal: None,
            },
            message: message.to_owned(),
        };

        app.screen_mode = crate::app::ScreenMode::Fullscreen;
        finish_prepare_error(&mut app, error());
        assert_eq!(
            app.agents[&id]
                .toast
                .as_ref()
                .map(|(text, _)| text.as_str()),
            Some(message)
        );

        app.agents.get_mut(&id).unwrap().toast = None;
        app.screen_mode = crate::app::ScreenMode::Minimal;
        finish_prepare_error(&mut app, error());
        assert!(
            app.agents[&id]
                .scrollback
                .iter_entries()
                .any(|(_, entry)| entry.block.searchable_text().as_deref() == Some(message))
        );
    }

    #[test]
    fn revalidation_cancels_after_warm_voice_ownership_change() {
        let (mut app, request) = app_with_prompt_request();
        let id = AgentId(0);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        app.voice_cmd_tx = Some(tx);
        app.voice_state = crate::app::app_view::VoiceState::Recording {
            hold: false,
            target: crate::app::app_view::VoiceTarget::Agent(id),
            interim: None,
        };
        assert!(prepare(&mut app, request).unwrap().is_none());
        assert!(
            app.agents[&id]
                .scrollback
                .iter_entries()
                .any(|(_, entry)| entry.block.searchable_text().as_deref() == Some(VOICE_MESSAGE))
        );
    }

    #[test]
    fn revalidation_cancels_after_cold_voice_ownership_change() {
        let (mut app, request) = app_with_prompt_request();
        let id = AgentId(0);
        app.voice_state = crate::app::app_view::VoiceState::ColdStart {
            hold: false,
            target: crate::app::app_view::VoiceTarget::Agent(id),
        };
        assert!(prepare(&mut app, request).unwrap().is_none());
        assert!(
            app.agents[&id]
                .scrollback
                .iter_entries()
                .any(|(_, entry)| entry.block.searchable_text().as_deref() == Some(VOICE_MESSAGE))
        );
    }

    #[test]
    fn revalidation_cancels_after_paste_probe_ownership_change() {
        let (mut app, request) = app_with_prompt_request();
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().paste_probe_in_flight = 1;
        assert!(prepare(&mut app, request).unwrap().is_none());
        assert!(
            app.agents[&id]
                .scrollback
                .iter_entries()
                .any(|(_, entry)| entry.block.searchable_text().as_deref() == Some(PASTE_MESSAGE))
        );
    }

    #[test]
    fn finish_success_failure_and_stale_paths_preserve_contracts() {
        let id = AgentId(0);
        let mut app = crate::app::app_view::tests::test_app();
        let mut agent = crate::test_util::make_agent_view(Some("session"), "/work");
        agent.session.id = id;
        agent.prompt.set_text("original");
        app.agents.insert(id, agent);
        app.active_view = ActiveView::Agent(id);

        let file = PromptEditorFile::create("edited\n").unwrap();
        let prepared = PreparedEditorRequest::PromptDraft {
            launch: EditorLaunch {
                argv: vec!["editor".to_owned()],
                path: file.path().to_path_buf(),
            },
            agent_id: id,
            original_text: "original".to_owned(),
            file,
        };
        finish(&mut app, prepared, Ok(success_status()));
        assert_eq!(
            app.agents[&id].prompt.text(),
            "edited",
            "the editor's final newline is stripped"
        );

        app.agents.get_mut(&id).unwrap().prompt.set_text("newer");
        apply_prompt_outcome(&mut app, id, "original".to_owned(), Ok("stale".to_owned()));
        assert_eq!(app.agents[&id].prompt.text(), "newer");

        apply_prompt_outcome(&mut app, id, "newer".to_owned(), Err(PROMPT_EDITOR_NONZERO));
        assert_eq!(app.agents[&id].prompt.text(), "newer");
        assert!(
            app.agents[&id]
                .scrollback
                .iter_entries()
                .any(|(_, entry)| entry.block.searchable_text().as_deref()
                    == Some(PROMPT_EDITOR_NONZERO))
        );
    }

    /// The trailing-newline strip discriminates editor-added from
    /// draft-owned: editors append a final newline only when the buffer
    /// doesn't already end with one, so a draft that ended with '\n' keeps
    /// it, and a blank line added in the editor to a newline-less draft
    /// survives as exactly one '\n' (only the editor-added one is stripped).
    #[test]
    fn finish_preserves_a_drafts_own_trailing_newline() {
        let id = AgentId(0);
        let mut app = crate::app::app_view::tests::test_app();
        let mut agent = crate::test_util::make_agent_view(Some("session"), "/work");
        agent.session.id = id;
        app.agents.insert(id, agent);
        app.active_view = ActiveView::Agent(id);

        // Draft ends with '\n': the file was saved as-is — no strip.
        app.agents.get_mut(&id).unwrap().prompt.set_text("line\n");
        let file = PromptEditorFile::create("edited\n").unwrap();
        let prepared = PreparedEditorRequest::PromptDraft {
            launch: EditorLaunch {
                argv: vec!["editor".to_owned()],
                path: file.path().to_path_buf(),
            },
            agent_id: id,
            original_text: "line\n".to_owned(),
            file,
        };
        finish(&mut app, prepared, Ok(success_status()));
        assert_eq!(
            app.agents[&id].prompt.text(),
            "edited\n",
            "a draft's own trailing newline survives the round trip"
        );

        // Newline-less draft, user appends a blank line: the editor
        // saved "text\n\n"; only the editor-added final '\n' is stripped.
        app.agents.get_mut(&id).unwrap().prompt.set_text("text");
        let file = PromptEditorFile::create("text\n\n").unwrap();
        let prepared = PreparedEditorRequest::PromptDraft {
            launch: EditorLaunch {
                argv: vec!["editor".to_owned()],
                path: file.path().to_path_buf(),
            },
            agent_id: id,
            original_text: "text".to_owned(),
            file,
        };
        finish(&mut app, prepared, Ok(success_status()));
        assert_eq!(
            app.agents[&id].prompt.text(),
            "text\n",
            "a deliberately added trailing blank line survives as one newline"
        );
    }

    #[test]
    fn unconsumed_prompt_request_has_no_materialized_path() {
        let request = PendingEditorRequest::PromptDraft {
            agent_id: AgentId(7),
            original_text: "sensitive draft".to_owned(),
        };
        assert!(matches!(
            request,
            PendingEditorRequest::PromptDraft {
                agent_id: AgentId(7),
                ref original_text,
            } if original_text == "sensitive draft"
        ));
        drop(request);
    }
}
