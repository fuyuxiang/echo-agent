use agent_client_protocol as acp;
use echo_agent_tools::implementations::echo_agent_build::{
    IMAGE_GEN_TOOL_NAME, IMAGINE_COMMAND_NAME, imagine_instruction, imagine_usage_message,
};

use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

const REQUIRED_TOOLS: &[&str] = &[IMAGE_GEN_TOOL_NAME];

pub struct ImagineCommand;

impl SlashCommand for ImagineCommand {
    fn name(&self) -> &str {
        IMAGINE_COMMAND_NAME
    }

    fn description(&self) -> &str {
        "Generate an image from a text description"
    }

    fn usage(&self) -> &str {
        "/imagine <description>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("description of the image to generate")
    }

    fn required_tools(&self) -> &[&str] {
        REQUIRED_TOOLS
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let prompt = args.trim();
        if prompt.is_empty() {
            return CommandResult::Message(imagine_usage_message().to_string());
        }

        CommandResult::InjectSkill {
            display_text: format!("/imagine {prompt}"),
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                imagine_instruction(prompt),
            ))],
            display_as_skill: false,
            scheduled_task_preview: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_image_gen_tool() {
        assert_eq!(ImagineCommand.required_tools(), &["image_gen"]);
    }

    #[test]
    fn empty_prompt_returns_usage() {
        let models = crate::acp::model_state::ModelState::default();
        let mut ctx = super::super::tests::make_ctx(&models);
        let result = ImagineCommand.run(&mut ctx, "");
        assert!(matches!(result, CommandResult::Message(_)));
    }

    #[test]
    fn whitespace_prompt_returns_usage() {
        let models = crate::acp::model_state::ModelState::default();
        let mut ctx = super::super::tests::make_ctx(&models);
        let result = ImagineCommand.run(&mut ctx, "   ");
        assert!(matches!(result, CommandResult::Message(_)));
    }

    #[test]
    fn valid_prompt_returns_inject_skill() {
        let models = crate::acp::model_state::ModelState::default();
        let mut ctx = super::super::tests::make_ctx(&models);
        let result = ImagineCommand.run(&mut ctx, "a golden sunset");
        match result {
            CommandResult::InjectSkill {
                display_text,
                prompt_blocks,
                display_as_skill,
                ..
            } => {
                assert_eq!(display_text, "/imagine a golden sunset");
                assert!(!display_as_skill);
                assert_eq!(prompt_blocks.len(), 1);
                let text = match &prompt_blocks[0] {
                    acp::ContentBlock::Text(t) => &t.text,
                    _ => panic!("expected Text block"),
                };
                assert!(text.contains("image_gen"));
                assert!(text.contains("a golden sunset"));
            }
            other => panic!("expected InjectSkill, got {other:?}"),
        }
    }
}
