//! Startup enforcement of the version policy. The hard `required_*` bounds gate
//! startup; `minimum`/`maximum` are updater-only. Every knob fails open.

use crate::version::get_installed_echo_agent_version;
use echo_agent_runtime::util::config::VersionPolicy;
use semver::Version;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
enum RequiredRangeDecision {
    InRange,
    Below { current: String, minimum: String },
    Above { current: String, maximum: String },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum VersionPolicyError {
    #[error(
        "Cannot install EchoAgent {target}: the minimum allowed version is {minimum}. \
         Run `echoagent update` to install the latest allowed version."
    )]
    TargetBelowFloor { target: String, minimum: String },
}

/// Fails open: a contradictory range or an unparseable running version yields
/// `InRange`.
fn evaluate_required_range(current_version: &str, policy: &VersionPolicy) -> RequiredRangeDecision {
    if policy.has_contradictory_required_range() {
        warn!(
            required_min = ?policy.required_minimum,
            required_max = ?policy.required_maximum,
            "required version range is contradictory (min > max); ignoring"
        );
        return RequiredRangeDecision::InRange;
    }

    let Ok(cur) = Version::parse(current_version) else {
        return RequiredRangeDecision::InRange;
    };

    if let Some(mn) = &policy.required_minimum
        && cur < *mn
    {
        return RequiredRangeDecision::Below {
            current: cur.to_string(),
            minimum: mn.to_string(),
        };
    }
    if let Some(mx) = &policy.required_maximum
        && cur > *mx
    {
        return RequiredRangeDecision::Above {
            current: cur.to_string(),
            maximum: mx.to_string(),
        };
    }
    RequiredRangeDecision::InRange
}

/// Reject an explicit `--version` pin below the hard floor. A pin above the
/// ceiling is allowed so a too-new install can recover.
pub(crate) fn check_install_target(
    policy: &VersionPolicy,
    target: &str,
) -> Result<(), VersionPolicyError> {
    let Some(min) = policy.installable_floor() else {
        return Ok(());
    };
    if !matches!(Version::parse(target), Ok(t) if t >= min) {
        return Err(VersionPolicyError::TargetBelowFloor {
            target: target.to_string(),
            minimum: min.to_string(),
        });
    }
    Ok(())
}

fn required_range_message(decision: &RequiredRangeDecision) -> Option<String> {
    match decision {
        RequiredRangeDecision::InRange => None,
        RequiredRangeDecision::Below { current, minimum } => Some(format!(
            "This version of EchoAgent ({current}) is older than the minimum required \
             by your organization ({minimum}).\n\n\
             Update to an approved version through your organization's approved \
             method (for example, run `echoagent update`)."
        )),
        RequiredRangeDecision::Above { current, maximum } => Some(format!(
            "This version of EchoAgent ({current}) is newer than the maximum allowed \
             by your organization ({maximum}).\n\n\
             Install an approved version through your organization's approved \
             method (for example, run `echoagent update --version {maximum}`)."
        )),
    }
}

/// Refuse to start when the running version is outside the required range.
/// Recovery subcommands return before this, so they stay usable.
pub fn enforce_version_policy_or_exit() {
    let policy = VersionPolicy::resolve();
    let current = get_installed_echo_agent_version();
    let decision = evaluate_required_range(&current, &policy);
    if let Some(message) = required_range_message(&decision) {
        warn!(?decision, "required version range: refusing to start");
        eprintln!("{message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn pol(
        min: Option<&str>,
        max: Option<&str>,
        rmin: Option<&str>,
        rmax: Option<&str>,
    ) -> VersionPolicy {
        VersionPolicy {
            minimum: min.map(v),
            maximum: max.map(v),
            required_minimum: rmin.map(v),
            required_maximum: rmax.map(v),
        }
    }

    #[test]
    fn check_install_target_enforces_only_the_hard_floor() {
        assert!(check_install_target(&pol(Some("0.2.100"), None, None, None), "0.2.50").is_ok());
        let hard = pol(None, None, Some("0.1.100"), None);
        assert!(check_install_target(&hard, "0.1.150").is_ok());
        assert!(matches!(
            check_install_target(&hard, "0.1.50").unwrap_err(),
            VersionPolicyError::TargetBelowFloor { .. }
        ));
        assert!(matches!(
            check_install_target(&hard, "garbage").unwrap_err(),
            VersionPolicyError::TargetBelowFloor { .. }
        ));
        assert!(check_install_target(&pol(None, None, None, None), "garbage").is_ok());
        assert!(
            check_install_target(&pol(None, None, Some("0.3.0"), Some("0.2.0")), "0.1.0").is_ok()
        );
        assert!(
            check_install_target(
                &pol(None, None, Some("0.2.100"), Some("0.2.150")),
                "0.2.200"
            )
            .is_ok()
        );
    }

    #[test]
    fn evaluate_required_range_gates_and_fails_open() {
        use RequiredRangeDecision::{Above, Below, InRange};

        assert_eq!(
            evaluate_required_range(
                "0.2.100",
                &pol(None, None, Some("0.2.100"), Some("0.2.150"))
            ),
            InRange
        );
        assert!(matches!(
            evaluate_required_range("0.2.99", &pol(None, None, Some("0.2.100"), None)),
            Below { .. }
        ));
        assert!(matches!(
            evaluate_required_range("0.2.200", &pol(None, None, None, Some("0.2.150"))),
            Above { .. }
        ));
        assert_eq!(
            evaluate_required_range("0.2.50", &pol(None, None, Some("0.3.0"), Some("0.2.0"))),
            InRange
        );
        assert_eq!(
            evaluate_required_range("dev-build", &pol(None, None, Some("0.2.100"), None)),
            InRange
        );
        assert_eq!(
            evaluate_required_range("0.2.50", &pol(Some("0.2.100"), None, None, None)),
            InRange
        );
    }

    #[test]
    fn required_range_message_is_none_only_when_in_range() {
        assert!(required_range_message(&RequiredRangeDecision::InRange).is_none());
        assert!(
            required_range_message(&RequiredRangeDecision::Below {
                current: "0.2.99".into(),
                minimum: "0.2.100".into(),
            })
            .is_some()
        );
        assert!(
            required_range_message(&RequiredRangeDecision::Above {
                current: "0.2.200".into(),
                maximum: "0.2.150".into(),
            })
            .is_some()
        );
    }
}
