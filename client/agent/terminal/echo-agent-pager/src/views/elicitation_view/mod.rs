//! MCP elicitation card (`echo.agent/mcp/elicit`): form and URL elicitation with
//! the same lifecycle as the other HITL cards (permission / question / plan
//! approval). State transitions live in [`state`], painting in [`render`].

mod render;
mod state;
#[cfg(test)]
mod tests;

pub use render::{ElicitHit, elicitation_view_height, render_elicitation_view};
pub use state::{
    ElicitResponseTx, ElicitationActionFocus, ElicitationFocus, ElicitationStage,
    ElicitationViewState, FieldValueUi, FormFieldUi, FormStage, UrlConsentStage, UrlDisplay,
    UrlWaitingStage,
};
