use async_trait::async_trait;

use crate::send::contributors::turn_input::{
    TurnInputContext, TurnInputContributor, TurnInputFragment,
};

/// `?Send` twin of [`TurnInputContributor`] for single-threaded hosts like echoagent build's TUI agent, whose session state is `Rc`/`RefCell`-based
/// and can never satisfy the `Send` bounds the send flavor bakes into its boxed hook futures.
#[async_trait(?Send)]
pub trait LocalTurnInputContributor {
    async fn contribute_turn_input(&self, _input: &TurnInputContext) -> Vec<TurnInputFragment> {
        Vec::new()
    }
}

/// Send contributors are usable in single-threaded hosts as-is, so shared logic implements [`TurnInputContributor`] once for both hosts.
#[async_trait(?Send)]
impl<T: TurnInputContributor> LocalTurnInputContributor for T {
    async fn contribute_turn_input(&self, input: &TurnInputContext) -> Vec<TurnInputFragment> {
        TurnInputContributor::contribute_turn_input(self, input).await
    }
}
