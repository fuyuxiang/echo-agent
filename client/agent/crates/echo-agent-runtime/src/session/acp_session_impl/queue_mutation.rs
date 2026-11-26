//! Internal authorization for generic prompt-queue controls.

use super::*;
use echo_agent_lifecycle::{InputPolicy, QueuePolicy, ShutdownPolicy};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputOrigin(PromptOrigin);

impl InputOrigin {
    pub(crate) fn from_prompt_id(prompt_id: &str) -> Self {
        Self(PromptOrigin::from_prompt_id(prompt_id))
    }

    pub(crate) const fn new(origin: PromptOrigin) -> Self {
        Self(origin)
    }

    pub(crate) fn policy(&self) -> InputPolicy {
        self.0.policy()
    }

    pub(crate) const fn as_prompt_origin(&self) -> &PromptOrigin {
        &self.0
    }

    pub(crate) fn is_synthetic(&self) -> bool {
        self.0.is_synthetic()
    }

    pub(crate) fn is_preemptible_runtime_wake(&self) -> bool {
        !matches!(self.policy().shutdown, ShutdownPolicy::Drain)
    }

    pub(crate) fn completion_id(&self) -> Option<&str> {
        self.0.completion_id()
    }
}

/// Positive-capability authorization for generic queue controls.
///
/// Represented as visibility/editability capabilities rather than an enum so a
/// protected (visible, non-editable) row can be minted without a dead variant on
/// lower stack layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QueueMutationPolicy {
    visible: bool,
    editable: bool,
}

impl QueueMutationPolicy {
    pub(crate) const fn new(visible: bool, editable: bool) -> Self {
        Self { visible, editable }
    }

    pub(crate) const fn editable() -> Self {
        Self::new(true, true)
    }

    pub(crate) const fn hidden() -> Self {
        Self::new(false, false)
    }

    pub(crate) fn is_visible(self) -> bool {
        self.visible
    }

    pub(crate) fn is_editable(self) -> bool {
        self.editable
    }

    pub(crate) fn is_protected(self) -> bool {
        self.visible && !self.editable
    }

    pub(crate) fn from_input_origin(origin: &InputOrigin) -> Self {
        match origin.policy().queue {
            QueuePolicy::VisibleProtected => Self::new(true, false),
            QueuePolicy::VisibleEditable => Self::editable(),
            QueuePolicy::Hidden => Self::hidden(),
        }
    }
}

impl InputItem {
    pub(crate) fn is_queue_visible(&self) -> bool {
        self.queue_mutation_policy.is_visible()
    }

    pub(crate) fn is_queue_editable(&self) -> bool {
        self.queue_mutation_policy.is_editable()
    }

    pub(crate) fn is_queue_protected(&self) -> bool {
        self.queue_mutation_policy.is_protected()
    }

    pub(crate) fn editable_queue_meta_matches(
        &self,
        id: &str,
        expected_version: u64,
        owner: Option<&str>,
    ) -> bool {
        self.is_queue_editable()
            && self.queue_meta.as_ref().is_some_and(|meta| {
                meta.id == id
                    && meta.version == expected_version
                    && owner.is_none_or(|expected| meta.owner.as_deref() == Some(expected))
            })
    }

    pub(crate) fn has_queue_id(&self, id: &str) -> bool {
        self.queue_meta.as_ref().is_some_and(|meta| meta.id == id)
    }
}
