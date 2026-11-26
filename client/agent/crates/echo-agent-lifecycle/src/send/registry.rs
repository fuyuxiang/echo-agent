use std::collections::HashMap;
use std::sync::Arc;

use crate::send::contributors::{
    CommandContributor, SessionLifecycleContributor, TurnInputContributor, TurnLifecycleContributor,
};

/// Mutable registry used while hosts register typed runtime contributions.
#[derive(Default)]
pub struct ExtensionRegistryBuilder {
    turn_lifecycle_contributors: Vec<Arc<dyn TurnLifecycleContributor>>,
    session_lifecycle_contributors: Vec<Arc<dyn SessionLifecycleContributor>>,
    turn_input_contributors: Vec<Arc<dyn TurnInputContributor>>,
    command_contributors: Vec<Arc<dyn CommandContributor>>,
}

impl ExtensionRegistryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn turn_lifecycle_contributor(&mut self, contributor: Arc<dyn TurnLifecycleContributor>) {
        self.turn_lifecycle_contributors.push(contributor);
    }

    pub fn session_lifecycle_contributor(
        &mut self,
        contributor: Arc<dyn SessionLifecycleContributor>,
    ) {
        self.session_lifecycle_contributors.push(contributor);
    }

    pub fn turn_input_contributor(&mut self, contributor: Arc<dyn TurnInputContributor>) {
        self.turn_input_contributors.push(contributor);
    }

    pub fn command_contributor(&mut self, contributor: Arc<dyn CommandContributor>) {
        self.command_contributors.push(contributor);
    }

    /// Routes each advertised command to its one owner. Duplicate names are a composition bug:
    /// first registration wins, panics in debug builds, logs in release.
    pub fn build(self) -> ExtensionRegistry {
        let mut command_handlers: HashMap<String, Arc<dyn CommandContributor>> = HashMap::new();
        for contributor in &self.command_contributors {
            for spec in contributor.advertised_commands() {
                if command_handlers.contains_key(&spec.name) {
                    debug_assert!(false, "duplicate command contributed: /{}", spec.name);
                    tracing::error!(command = %spec.name, "Duplicate command contributed; first registration wins");
                    continue;
                }
                command_handlers.insert(spec.name, contributor.clone());
            }
        }

        ExtensionRegistry {
            turn_lifecycle_contributors: self.turn_lifecycle_contributors,
            session_lifecycle_contributors: self.session_lifecycle_contributors,
            turn_input_contributors: self.turn_input_contributors,
            command_contributors: self.command_contributors,
            command_handlers,
        }
    }
}

/// Immutable typed registry produced after extensions are installed.
#[derive(Default)]
pub struct ExtensionRegistry {
    turn_lifecycle_contributors: Vec<Arc<dyn TurnLifecycleContributor>>,
    session_lifecycle_contributors: Vec<Arc<dyn SessionLifecycleContributor>>,
    turn_input_contributors: Vec<Arc<dyn TurnInputContributor>>,
    command_contributors: Vec<Arc<dyn CommandContributor>>,
    command_handlers: HashMap<String, Arc<dyn CommandContributor>>,
}

impl ExtensionRegistry {
    pub fn turn_lifecycle_contributors(&self) -> &[Arc<dyn TurnLifecycleContributor>] {
        &self.turn_lifecycle_contributors
    }

    pub fn session_lifecycle_contributors(&self) -> &[Arc<dyn SessionLifecycleContributor>] {
        &self.session_lifecycle_contributors
    }

    pub fn turn_input_contributors(&self) -> &[Arc<dyn TurnInputContributor>] {
        &self.turn_input_contributors
    }

    pub fn command_contributors(&self) -> &[Arc<dyn CommandContributor>] {
        &self.command_contributors
    }

    /// The one contributor owning `name`, or `None` when no extension advertised it.
    pub fn command_handler(&self, name: &str) -> Option<&Arc<dyn CommandContributor>> {
        self.command_handlers.get(name)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::send::contributors::{
        AnalyticsClass, CommandAction, CommandInvocation, CommandSpec, CompactionClass,
        InputAuthority, InputPolicy, QueuePolicy, SessionIdleInput, ShutdownPolicy, TurnAbortInput,
        TurnAbortReason, TurnBoundary, TurnDoneInput, TurnErrorInput, TurnInputContext,
        TurnInputFragment, TurnStartInput,
    };

    struct Counter(AtomicUsize);

    #[async_trait]
    impl TurnLifecycleContributor for Counter {
        async fn on_turn_done(&self, _input: &TurnDoneInput) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct LegacyStartCounter(AtomicUsize);

    #[async_trait]
    impl TurnLifecycleContributor for LegacyStartCounter {
        async fn on_turn_start(&self, _input: &TurnStartInput) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct TypedCounter(AtomicUsize);

    #[async_trait]
    impl TurnLifecycleContributor for TypedCounter {
        async fn on_turn_start_with_policy(&self, _input: &TurnStartInput, policy: InputPolicy) {
            assert_eq!(policy.authority, InputAuthority::ModelAuthoredUntrusted);
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl SessionLifecycleContributor for Counter {
        async fn on_session_idle(&self, _input: &SessionIdleInput) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl TurnInputContributor for Counter {
        async fn contribute_turn_input(&self, _input: &TurnInputContext) -> Vec<TurnInputFragment> {
            self.0.fetch_add(1, Ordering::SeqCst);
            vec![TurnInputFragment {
                text: "nudge".to_string(),
            }]
        }
    }

    #[async_trait]
    impl CommandContributor for Counter {
        fn advertised_commands(&self) -> Vec<CommandSpec> {
            vec![CommandSpec {
                name: "goal".to_string(),
                description: "Set a goal".to_string(),
                arg_hint: "<text>".to_string(),
            }]
        }

        async fn handle_command(
            &self,
            input: &CommandInvocation<'_>,
        ) -> Result<CommandAction, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(CommandAction::Rewrite {
                model_text: format!("{} {}", input.name, input.args),
            })
        }
    }

    #[test]
    #[should_panic(expected = "duplicate command contributed: /goal")]
    fn build_rejects_duplicate_command_names() {
        let counter = Arc::new(Counter(AtomicUsize::new(0)));
        let mut builder = ExtensionRegistryBuilder::new();
        builder.command_contributor(counter.clone());
        builder.command_contributor(counter);
        builder.build();
    }

    #[tokio::test]
    async fn typed_turn_start_defaults_to_legacy_and_can_be_overridden() {
        let policy = InputPolicy {
            authority: InputAuthority::ModelAuthoredUntrusted,
            turn_boundary: TurnBoundary::Conversational,
            analytics: AnalyticsClass::AgentMessage,
            compaction: CompactionClass::ConversationalAgentAnchor,
            queue: QueuePolicy::VisibleProtected,
            shutdown: ShutdownPolicy::Drain,
        };
        let legacy = LegacyStartCounter(AtomicUsize::new(0));
        legacy
            .on_turn_start_with_policy(&TurnStartInput::new(true), policy)
            .await;
        assert_eq!(legacy.0.load(Ordering::SeqCst), 1);

        let typed = TypedCounter(AtomicUsize::new(0));
        typed
            .on_turn_start_with_policy(&TurnStartInput::new(true), policy)
            .await;
        assert_eq!(typed.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn builder_freezes_and_registry_dispatches_in_order() {
        let counter = Arc::new(Counter(AtomicUsize::new(0)));
        let mut builder = ExtensionRegistryBuilder::new();
        builder.turn_lifecycle_contributor(counter.clone());
        builder.turn_lifecycle_contributor(counter.clone());
        builder.session_lifecycle_contributor(counter.clone());
        builder.turn_input_contributor(counter.clone());
        builder.command_contributor(counter.clone());
        let registry = builder.build();

        for contributor in registry.turn_lifecycle_contributors() {
            contributor
                .on_turn_start(&TurnStartInput { synthetic: false })
                .await;
            contributor.on_turn_done(&TurnDoneInput).await;
            contributor
                .on_turn_abort(&TurnAbortInput {
                    reason: TurnAbortReason::Interrupted,
                })
                .await;
            contributor
                .on_turn_error(&TurnErrorInput { message: "boom" })
                .await;
        }

        for contributor in registry.session_lifecycle_contributors() {
            contributor.on_session_idle(&SessionIdleInput).await;
        }

        for contributor in registry.turn_input_contributors() {
            let fragments = contributor
                .contribute_turn_input(&TurnInputContext {
                    turn_id: "turn-1".to_string(),
                    synthetic: false,
                })
                .await;
            assert_eq!(1, fragments.len());
            assert_eq!("nudge", fragments[0].text);
        }

        assert!(registry.command_handler("nope").is_none());
        let handler = registry.command_handler("goal").expect("goal has an owner");
        let action = handler
            .handle_command(&CommandInvocation {
                name: "goal",
                args: "ship it",
            })
            .await
            .expect("command should be handled");
        let CommandAction::Rewrite { model_text } = action else {
            panic!("expected a rewrite");
        };
        assert_eq!("goal ship it", model_text);

        assert_eq!(5, counter.0.load(Ordering::SeqCst));
    }
}
