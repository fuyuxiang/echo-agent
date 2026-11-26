use async_trait::async_trait;

/// A slash command a contributor advertises; the host maps it onto its own advertising protocol.
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    pub arg_hint: String,
}

/// A parsed `/name args` invocation. The host owns parsing and routes it to the command's one owner.
pub struct CommandInvocation<'a> {
    pub name: &'a str,
    pub args: &'a str, // Whitespace-trimmed; empty for a bare `/name`.
}

/// What a handled command does to the turn; rejections travel as the `Err` reason.
pub enum CommandAction {
    /// Replace the model-visible copy of the message with `model_text`.
    Rewrite { model_text: String },
    /// Side effect performed, nothing to say; the state change surfaces through the host's own rendering.
    Acted,
}

/// Handles the slash commands the extension advertises. Only invoked for commands this contributor
/// owns; the `Err` reason is the only channel for "why not", so hosts must surface it.
#[async_trait]
pub trait CommandContributor: Send + Sync {
    fn advertised_commands(&self) -> Vec<CommandSpec>;

    async fn handle_command(&self, _input: &CommandInvocation<'_>)
    -> Result<CommandAction, String>;
}
