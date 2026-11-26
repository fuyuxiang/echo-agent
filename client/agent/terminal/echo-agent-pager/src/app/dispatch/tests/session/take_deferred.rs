use crate::acp::model_state::{EffortTokenError, ModelState};
use crate::app::agent::DeferredModelSwitch;
use crate::app::dispatch::session::lifecycle::{DeferredSwitchOutcome, take_deferred_model_switch};
use agent_client_protocol as acp;
use echo_agent_runtime::sampling::types::ReasoningEffort;
use std::sync::Arc;

fn model_with_support(id: &str, supports: bool) -> (acp::ModelId, acp::ModelInfo) {
    let id = acp::ModelId::new(Arc::from(id));
    let meta = if supports {
        Some(serde_json::json!({
            "supportsReasoningEffort": true,
            "reasoningEffort": "medium",
            "reasoningEfforts": [
                { "id": "deep", "value": "xhigh", "label": "Deep" },
                { "id": "high", "value": "high", "label": "High" },
            ],
        }))
    } else {
        Some(serde_json::json!({ "reasoningEffort": "medium" }))
    };
    let info = acp::ModelInfo::new(id.clone(), id.0.to_string())
        .meta(meta.and_then(|v| v.as_object().cloned()));
    (id, info)
}

fn models_with_current(supports: bool) -> ModelState {
    let (id, info) = model_with_support("echo-agent-build", supports);
    let mut models = ModelState::default();
    models.available.insert(id.clone(), info);
    models.current = Some(id);
    models.reasoning_effort = Some(ReasoningEffort::Medium);
    models
}

#[test]
fn effort_only_resolves_canonical_token() {
    let models = models_with_current(true);
    let out = take_deferred_model_switch(None, &models, Some("high"));
    assert_eq!(
        out,
        DeferredSwitchOutcome {
            switch: Some(switch(
                models.current.clone().unwrap(),
                Some(ReasoningEffort::High)
            )),
            effort_error: None,
        }
    );
}

#[test]
fn effort_only_resolves_remapped_menu_id() {
    let models = models_with_current(true);
    let out = take_deferred_model_switch(None, &models, Some("deep"));
    assert_eq!(
        out,
        DeferredSwitchOutcome {
            switch: Some(switch(
                models.current.clone().unwrap(),
                Some(ReasoningEffort::Xhigh)
            )),
            effort_error: None,
        }
    );
}

#[test]
fn effort_only_unsupported_canonical_token_is_unsupported() {
    // Gate-first: a canonical token on a model that doesn't support reasoning
    // effort surfaces Unsupported (matching `/effort` and headless) rather than
    // silently applying an effort the server would drop.
    let models = models_with_current(false);
    assert_eq!(
        take_deferred_model_switch(None, &models, Some("high")),
        DeferredSwitchOutcome {
            switch: None,
            effort_error: Some(EffortTokenError::Unsupported),
        }
    );
}

#[test]
fn effort_only_unsupported_unknown_token_is_unsupported() {
    let models = models_with_current(false);
    assert_eq!(
        take_deferred_model_switch(None, &models, Some("bogus")),
        DeferredSwitchOutcome {
            switch: None,
            effort_error: Some(EffortTokenError::Unsupported),
        }
    );
}

#[test]
fn effort_only_skips_when_already_equal() {
    let mut models = models_with_current(true);
    models.reasoning_effort = Some(ReasoningEffort::High);
    assert_eq!(
        take_deferred_model_switch(None, &models, Some("high")),
        DeferredSwitchOutcome {
            switch: None,
            effort_error: None,
        }
    );
}

#[test]
fn effort_only_errors_on_unknown_token() {
    let models = models_with_current(true);
    assert_eq!(
        take_deferred_model_switch(None, &models, Some("bogus")),
        DeferredSwitchOutcome {
            switch: None,
            effort_error: Some(EffortTokenError::UnknownToken {
                token: "bogus".into(),
                offered: vec!["deep".into(), "high".into()],
            }),
        }
    );
}

#[test]
fn stashed_model_switch_prefers_explicit_stash() {
    let models = models_with_current(true);
    let other = acp::ModelId::new(Arc::from("other-model"));
    let out = take_deferred_model_switch(
        Some(switch(other.clone(), Some(ReasoningEffort::Low))),
        &models,
        Some("high"),
    );
    assert_eq!(
        out,
        DeferredSwitchOutcome {
            switch: Some(switch(other, Some(ReasoningEffort::Low))),
            effort_error: None,
        }
    );
}

#[test]
fn stashed_model_re_resolves_remap_when_effort_missing() {
    let models = models_with_current(true);
    let current = models.current.clone().unwrap();
    let out =
        take_deferred_model_switch(Some(switch(current.clone(), None)), &models, Some("deep"));
    assert_eq!(
        out,
        DeferredSwitchOutcome {
            switch: Some(switch(current, Some(ReasoningEffort::Xhigh))),
            effort_error: None,
        }
    );
}

#[test]
fn stashed_model_keeps_model_when_token_unresolvable() {
    let models = models_with_current(true);
    let current = models.current.clone().unwrap();
    let out =
        take_deferred_model_switch(Some(switch(current.clone(), None)), &models, Some("bogus"));
    assert_eq!(
        out,
        DeferredSwitchOutcome {
            switch: Some(switch(current, None)),
            effort_error: Some(EffortTokenError::UnknownToken {
                token: "bogus".into(),
                offered: vec!["deep".into(), "high".into()],
            }),
        }
    );
}

#[test]
fn stashed_model_keeps_model_when_unsupported() {
    // -m targets a non-reasoning model plus an effort token: keep the model
    // switch, drop the effort, and surface Unsupported.
    let mut models = models_with_current(true);
    let (plain, plain_info) = model_with_support("plain-model", false);
    models.available.insert(plain.clone(), plain_info);
    let out = take_deferred_model_switch(Some(switch(plain.clone(), None)), &models, Some("high"));
    assert_eq!(
        out,
        DeferredSwitchOutcome {
            switch: Some(switch(plain, None)),
            effort_error: Some(EffortTokenError::Unsupported),
        }
    );
}

#[test]
fn effort_only_rejects_max_when_model_does_not_offer_it() {
    let models = models_with_current(true);
    let out = take_deferred_model_switch(None, &models, Some("max"));
    assert_eq!(
        out,
        DeferredSwitchOutcome {
            switch: None,
            effort_error: Some(EffortTokenError::UnknownToken {
                token: "max".into(),
                offered: vec!["deep".into(), "high".into()],
            }),
        }
    );
}

#[test]
fn effort_only_errors_without_active_model() {
    let models = ModelState::default();
    assert_eq!(
        take_deferred_model_switch(None, &models, Some("high")),
        DeferredSwitchOutcome {
            switch: None,
            effort_error: Some(EffortTokenError::NoActiveModel),
        }
    );
}

/// Stash/outcome with no rollback target (prev threading is covered by the
/// router/lifecycle tests).
fn switch(model_id: acp::ModelId, effort: Option<ReasoningEffort>) -> DeferredModelSwitch {
    DeferredModelSwitch {
        model_id,
        effort,
        prev_model_id: None,
    }
}
