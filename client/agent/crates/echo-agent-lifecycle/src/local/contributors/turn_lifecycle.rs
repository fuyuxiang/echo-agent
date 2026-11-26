use async_trait::async_trait;

use crate::send::contributors::turn_lifecycle::{
    InputPolicy, TurnAbortInput, TurnDoneInput, TurnErrorInput, TurnLifecycleContributor,
    TurnStartInput,
};

/// `?Send` twin of [`TurnLifecycleContributor`] for single-threaded hosts like echoagent build's TUI
/// agent, whose session state is `Rc`/`RefCell`-based and can never satisfy the `Send` bounds the
/// send flavor bakes into its boxed hook futures.
#[async_trait(?Send)]
pub trait LocalTurnLifecycleContributor {
    async fn on_turn_start(&self, _input: &TurnStartInput) {}

    async fn on_turn_start_with_policy(&self, input: &TurnStartInput, _policy: InputPolicy) {
        self.on_turn_start(input).await;
    }

    async fn on_turn_done(&self, _input: &TurnDoneInput) {}

    async fn on_turn_abort(&self, _input: &TurnAbortInput) {}

    async fn on_turn_error(&self, _input: &TurnErrorInput<'_>) {}
}

/// Send contributors are usable in single-threaded hosts as-is, so shared logic implements
/// [`TurnLifecycleContributor`] once and both hosts can register it.
#[async_trait(?Send)]
impl<T: TurnLifecycleContributor> LocalTurnLifecycleContributor for T {
    async fn on_turn_start(&self, input: &TurnStartInput) {
        TurnLifecycleContributor::on_turn_start(self, input).await;
    }

    async fn on_turn_start_with_policy(&self, input: &TurnStartInput, policy: InputPolicy) {
        TurnLifecycleContributor::on_turn_start_with_policy(self, input, policy).await;
    }

    async fn on_turn_done(&self, input: &TurnDoneInput) {
        TurnLifecycleContributor::on_turn_done(self, input).await;
    }

    async fn on_turn_abort(&self, input: &TurnAbortInput) {
        TurnLifecycleContributor::on_turn_abort(self, input).await;
    }

    async fn on_turn_error(&self, input: &TurnErrorInput<'_>) {
        TurnLifecycleContributor::on_turn_error(self, input).await;
    }
}
