use async_trait::async_trait;

use crate::send::contributors::command::{
    CommandAction, CommandContributor, CommandInvocation, CommandSpec,
};

/// `?Send` twin of [`CommandContributor`] for single-threaded hosts like echoagent build's TUI agent, whose session state is `Rc`/`RefCell`-based and can
/// never satisfy the `Send` bounds the send flavor bakes into its boxed hook futures.
#[async_trait(?Send)]
pub trait LocalCommandContributor {
    fn advertised_commands(&self) -> Vec<CommandSpec>;

    async fn handle_command(&self, _input: &CommandInvocation<'_>)
    -> Result<CommandAction, String>;
}

/// Send contributors work in single-threaded hosts as-is, so shared logic implements [`CommandContributor`] once and both hosts can register it.
#[async_trait(?Send)]
impl<T: CommandContributor> LocalCommandContributor for T {
    fn advertised_commands(&self) -> Vec<CommandSpec> {
        CommandContributor::advertised_commands(self)
    }

    async fn handle_command(&self, input: &CommandInvocation<'_>) -> Result<CommandAction, String> {
        CommandContributor::handle_command(self, input).await
    }
}
