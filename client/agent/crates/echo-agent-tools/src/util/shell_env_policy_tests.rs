use super::{
    EnvironmentVariablePattern, ShellEnvironmentPolicy, ShellEnvironmentPolicyInherit,
    apply_shell_environment_policy, create_env_from_vars,
};
use std::collections::HashMap;

fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn patterns(globs: &[&str]) -> Vec<EnvironmentVariablePattern> {
    globs
        .iter()
        .map(|g| EnvironmentVariablePattern::new_case_insensitive(g))
        .collect()
}

#[test]
fn apply_policy_reshapes_command_env() {
    let mut set = HashMap::new();
    set.insert("MY_FLAG".to_string(), "1".to_string());
    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        set,
        ..Default::default()
    };
    let mut cmd = tokio::process::Command::new("true");
    apply_shell_environment_policy(&mut cmd, Some(&policy));
    let envs: HashMap<String, String> = cmd
        .as_std()
        .get_envs()
        .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
        .collect();
    assert_eq!(envs.get("MY_FLAG").map(String::as_str), Some("1"));
    // inherit=None cleared the env, so no inherited PATH leaks through.
    assert!(!envs.contains_key("PATH"));
}

#[test]
fn apply_noop_or_absent_policy_leaves_command_untouched() {
    let mut cmd = tokio::process::Command::new("true");
    apply_shell_environment_policy(&mut cmd, None);
    apply_shell_environment_policy(&mut cmd, Some(&ShellEnvironmentPolicy::default()));
    // No env_clear and no sets: the command carries no explicit env entries.
    assert_eq!(cmd.as_std().get_envs().count(), 0);
}

#[test]
fn default_excludes_drop_secrets_when_enabled() {
    let policy = ShellEnvironmentPolicy {
        ignore_default_excludes: false,
        ..Default::default()
    };
    assert!(!policy.is_noop());
    let env = create_env_from_vars(
        vars(&[
            ("PATH", "/bin"),
            ("MY_API_KEY", "x"),
            ("MY_SECRET", "y"),
            ("GH_TOKEN", "z"),
        ]),
        &policy,
    );
    assert_eq!(env.get("PATH").map(String::as_str), Some("/bin"));
    assert!(!env.contains_key("MY_API_KEY"));
    assert!(!env.contains_key("MY_SECRET"));
    assert!(!env.contains_key("GH_TOKEN"));
}

#[test]
fn inherit_none_starts_empty_then_set_applies() {
    let mut set = HashMap::new();
    set.insert("PATH".to_string(), "/usr/bin".to_string());
    set.insert("MY_FLAG".to_string(), "1".to_string());
    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        set,
        ..Default::default()
    };
    let env = create_env_from_vars(vars(&[("PATH", "/bin"), ("HOME", "/root")]), &policy);
    assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(env.get("MY_FLAG").map(String::as_str), Some("1"));
    assert!(!env.contains_key("HOME"));
}

#[test]
fn inherit_core_keeps_only_core_vars() {
    let policy = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::Core,
        ..Default::default()
    };
    let env = create_env_from_vars(vars(&[("PATH", "/bin"), ("RANDOM_VAR", "v")]), &policy);
    assert_eq!(env.get("PATH").map(String::as_str), Some("/bin"));
    assert!(!env.contains_key("RANDOM_VAR"));
}

#[test]
fn exclude_and_include_only_filter() {
    let policy = ShellEnvironmentPolicy {
        exclude: patterns(&["AWS_*"]),
        include_only: patterns(&["PATH", "HOME"]),
        ..Default::default()
    };
    let env = create_env_from_vars(
        vars(&[
            ("PATH", "/bin"),
            ("HOME", "/root"),
            ("AWS_SECRET", "s"),
            ("OTHER", "o"),
        ]),
        &policy,
    );
    assert_eq!(env.get("PATH").map(String::as_str), Some("/bin"));
    assert_eq!(env.get("HOME").map(String::as_str), Some("/root"));
    assert!(!env.contains_key("AWS_SECRET"));
    assert!(!env.contains_key("OTHER"));
}

#[test]
fn allows_filters_by_name_case_insensitively() {
    let policy = ShellEnvironmentPolicy {
        exclude: patterns(&["aws_*"]), // lowercase pattern, uppercase var
        include_only: patterns(&["PATH", "HOME"]),
        ..Default::default()
    };
    assert!(policy.allows("PATH"));
    assert!(!policy.allows("AWS_SECRET")); // excluded (case-insensitive)
    assert!(!policy.allows("OTHER")); // not in include_only

    let scrub = ShellEnvironmentPolicy {
        ignore_default_excludes: false,
        ..Default::default()
    };
    assert!(!scrub.allows("my_api_key")); // `*KEY*` matches case-insensitively
    assert!(ShellEnvironmentPolicy::default().allows("MY_API_KEY")); // default allows all
}

#[test]
fn allows_with_inherit_honors_inherit() {
    // inherit = none admits nothing.
    let none = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        ..Default::default()
    };
    assert!(!none.allows_with_inherit("PATH"));
    assert!(!none.allows_with_inherit("FOO"));

    // inherit = core admits only core names.
    let core = ShellEnvironmentPolicy {
        inherit: ShellEnvironmentPolicyInherit::Core,
        ..Default::default()
    };
    assert!(core.allows_with_inherit("PATH"));
    assert!(!core.allows_with_inherit("RANDOM_VAR"));

    // inherit = all defers to `allows` (exclude still applies).
    let all = ShellEnvironmentPolicy {
        exclude: patterns(&["AWS_*"]),
        ..Default::default()
    };
    assert!(all.allows_with_inherit("RANDOM_VAR"));
    assert!(!all.allows_with_inherit("AWS_SECRET"));
}
