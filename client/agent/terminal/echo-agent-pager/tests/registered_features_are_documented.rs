//! The registry is the source of truth and the generated configuration
//! reference is its operator-facing mirror. Keep the two surfaces in sync.

use echo_agent_runtime::agent::config::FEATURES;

const CONFIG_REFERENCE: &str = include_str!("../docs/user-guide/26-config-reference.md");

#[test]
fn every_registered_feature_reaches_the_operator() {
    for spec in FEATURES {
        assert!(
            CONFIG_REFERENCE.contains(&format!("`{}`", spec.path)),
            "{} has no row in the configuration reference",
            spec.path,
        );
        assert!(
            CONFIG_REFERENCE.contains(&format!("`{}`", spec.env)),
            "{} is undocumented in the configuration reference",
            spec.env,
        );
    }
}
