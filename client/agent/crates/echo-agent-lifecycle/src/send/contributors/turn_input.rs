use async_trait::async_trait;

/// Turn facts supplied when the host pulls extension input at its sampling chokepoint.
pub struct TurnInputContext {
    /// Stable host-owned turn identifier.
    pub turn_id: String,
    /// True when the harness produced the turn (auto-wake, drain, cron, continuation), not the user.
    pub synthetic: bool,
}

/// A model-visible input fragment contributed into the active turn. The host owns wrapping, origin stamping, and placement.
pub struct TurnInputFragment {
    pub text: String,
}

/// Contributes model-visible input fragments into the active turn when the host pulls at its sampling chokepoint.
/// Fragments land in the same turn, never a new one.
#[async_trait]
pub trait TurnInputContributor: Send + Sync {
    async fn contribute_turn_input(&self, _input: &TurnInputContext) -> Vec<TurnInputFragment> {
        Vec::new()
    }
}
