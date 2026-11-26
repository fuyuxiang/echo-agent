//! Pure dispatch preparation for external prompt editing.

use crate::app::actions::Effect;
use crate::app::agent_view::ExternalPromptEditorAccess;
use crate::app::app_view::{ActiveView, AppView, VoiceTarget};
use crate::app::external_editor::{
    ATTACHMENT_MESSAGE, PASTE_MESSAGE, PendingEditorRequest, VOICE_MESSAGE, report_prompt_failure,
};

pub(super) fn dispatch_edit_prompt_external(app: &mut AppView) -> Vec<Effect> {
    if app.pending_editor.is_some() {
        return vec![];
    }
    let ActiveView::Agent(agent_id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get(&agent_id) else {
        return vec![];
    };
    let access = agent.external_prompt_editor_access();
    if app.voice_recording_target() == Some(VoiceTarget::Agent(agent_id)) {
        report_prompt_failure(app, agent_id, VOICE_MESSAGE);
        return vec![];
    }
    match access {
        ExternalPromptEditorAccess::OwnedElsewhere => return vec![],
        ExternalPromptEditorAccess::PastePending => {
            report_prompt_failure(app, agent_id, PASTE_MESSAGE);
            return vec![];
        }
        ExternalPromptEditorAccess::Attachments => {
            report_prompt_failure(app, agent_id, ATTACHMENT_MESSAGE);
            return vec![];
        }
        ExternalPromptEditorAccess::Ready => {}
    }

    app.pending_editor = Some(PendingEditorRequest::PromptDraft {
        agent_id,
        original_text: app.agents[&agent_id].prompt.text().to_owned(),
    });
    vec![]
}
