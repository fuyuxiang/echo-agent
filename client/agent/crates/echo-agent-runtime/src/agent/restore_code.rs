//! Thin wire-format adapter that wraps the shared
//! [`echo_agent_workspace::session::git::build_restore_decision`] helper
//! into the JSON shape emitted by `LoadSession` on `_meta.codeRestore`.
use echo_agent_workspace::session::git::{
    CheckoutSessionOutcome, RestoreKind, build_restore_decision,
};
use serde_json::Value;
/// Build the `codeRestore` JSON meta, or `None` when no restore should
/// be reported (no checkout AND no archive applied). The shared
/// [`build_restore_decision`] is the source of truth; this function
/// only adapts the result into the wire JSON shape used by the
/// non-worktree path.
pub(crate) fn build_code_restore_meta(
    target_sha: &str,
    outcome: &CheckoutSessionOutcome,
    kind: RestoreKind,
) -> Option<Value> {
    let decision = build_restore_decision(Some(target_sha), outcome, kind);
    let summary = decision.summary?;
    Some(serde_json::json!({
        "restored": decision.restored,
        "summary": summary,
        "degree": decision.degree,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn outcome(
        checked_out: bool,
        stash_ref: Option<&str>,
        skipped: Option<&str>,
    ) -> CheckoutSessionOutcome {
        CheckoutSessionOutcome {
            checked_out,
            stash_ref: stash_ref.map(str::to_owned),
            stash_skipped_reason: skipped.map(str::to_owned),
        }
    }
    #[test]
    fn checkout_failed_emits_restored_false_meta() {
        let meta = build_code_restore_meta(
            "0123456789abcdef",
            &outcome(false, None, Some("MERGE_HEAD present")),
            RestoreKind::RegistryOff,
        )
        .unwrap();
        assert_eq!(meta["restored"], false);
        assert!(meta["degree"].is_null());
        let s = meta["summary"].as_str().unwrap();
        assert!(s.contains("restore aborted"));
        assert!(s.contains("MERGE_HEAD present"));
    }
}
