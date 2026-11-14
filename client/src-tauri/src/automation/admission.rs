//! Ephemeral binding between a real user turn and model-selected capabilities.
//! Never persisted or constructed from MCP arguments, history, or tool content.
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Selection {
    pub turn_id: String,
    pub mode: Capability,
    pub task: TaskChoice,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    BrowserUse,
    ComputerUse,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskChoice {
    New,
    Continue,
}

#[derive(Clone)]
pub struct UserTurn {
    pub epoch: u64,
    pub id: String,
    pub text: String,
    pub prompt_id: Option<String>,
    pub readonly: bool,
    pub task_generation: Option<u64>,
    pub previous_scope: Option<String>,
    pub previous_scope_incomplete: bool,
    pub scope_instruction: String,
}

impl UserTurn {
    pub fn new(epoch: u64, text: &str, prompt_id: Option<&str>, readonly: bool) -> Self {
        Self {
            epoch,
            id: uuid::Uuid::now_v7().to_string(),
            text: text.to_owned(),
            prompt_id: prompt_id.map(str::to_owned),
            readonly,
            task_generation: None,
            previous_scope: None,
            previous_scope_incomplete: false,
            scope_instruction: text.to_owned(),
        }
    }

    /// Persist the message's constraints immediately, before model inference
    /// or a crash. Selection later replaces only this passive amendment with
    /// the selected instruction, without appending or interpreting it twice.
    pub fn stage_task(&mut self, task: &mut super::task::TaskExecution) {
        self.previous_scope = task.scope.clone();
        self.previous_scope_incomplete = task.scope_incomplete;
        self.scope_instruction = task.retain_unselected_instruction(&self.text);
    }

    pub fn validate(&self, id: &str, epoch: u64) -> Result<(), String> {
        if self.id != id || self.epoch != epoch {
            return Err("cancelled: automation selection belongs to an expired user turn".into());
        }
        if self.readonly {
            return Err("permission_denied: this user turn is read-only".into());
        }
        if self.text.trim().is_empty() {
            return Err("input_required: describe the task before using automation".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_staged_without_classifying_words_or_granting_execution() {
        for text in [
            "帮我把刚才那个窗口收拾好",
            "检查代码",
            "解释如何操作微信",
            "不要保存",
        ] {
            let turn = UserTurn::new(7, text, Some("prompt"), false);
            assert_eq!(turn.text, text);
            assert!(turn.task_generation.is_none());
            assert!(turn.validate(&turn.id, 7).is_ok());
        }
    }

    #[test]
    fn old_turns_other_sessions_pause_and_readonly_cannot_select_capabilities() {
        let turn = UserTurn::new(7, "任务", None, false);
        let other = UserTurn::new(7, "任务", None, false);
        assert!(turn.validate(&other.id, 7).is_err());
        assert!(turn.validate(&turn.id, 8).is_err());
        let readonly = UserTurn::new(7, "操作电脑", None, true);
        assert!(readonly.validate(&readonly.id, 7).is_err());
    }

    #[test]
    fn selection_accepts_only_capability_and_task_choice_never_model_scope_or_authority() {
        let selection = serde_json::json!({"turnId":"turn", "mode":"computer_use", "task":"new"});
        assert!(serde_json::from_value::<Selection>(selection.clone()).is_ok());
        for field in ["scope", "authorized", "permission", "userText", "targets"] {
            let mut injected = selection.clone();
            injected[field] = serde_json::json!(true);
            assert!(serde_json::from_value::<Selection>(injected).is_err());
        }
        for (field, value) in [("mode", "default"), ("task", "resume")] {
            let mut invalid = selection.clone();
            invalid[field] = serde_json::json!(value);
            assert!(serde_json::from_value::<Selection>(invalid).is_err());
        }
    }

    #[test]
    fn unselected_constraints_are_durable_before_model_response_and_never_grant_execution() {
        let mut task = super::super::task::TaskExecution::new(false, Some("在微信发送报告".into()));
        let mut turn = UserTurn::new(7, "不要发送，先解释一下", None, true);
        turn.stage_task(&mut task);
        let checkpoint: super::super::task::TaskExecution =
            serde_json::from_value(serde_json::to_value(&task).unwrap()).unwrap();
        assert!(
            super::super::policy::effect_allowed(checkpoint.scope.as_deref().unwrap(), "send")
                .is_err()
        );
        assert!(!checkpoint.authorized);
        assert_eq!(turn.previous_scope.as_deref(), Some("在微信发送报告"));
        assert!(turn.validate(&turn.id, 7).is_err());
    }
}
