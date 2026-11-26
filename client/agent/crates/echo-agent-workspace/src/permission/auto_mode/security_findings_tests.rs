use super::*;
use ClassifierSecurityFinding::*;

const ALL: [ClassifierSecurityFinding; 9] = [
    FailClosedPolicy,
    UnparseableShell,
    OpaqueShell,
    ExecOrAmbientGit,
    EnvInjection,
    UnvettedEnv,
    FileWrite,
    DangerousCommand,
    SpecialExecSurface,
];

#[test]
fn tokens_are_stable_and_unique() {
    let tokens: Vec<&str> = ALL.iter().map(|f| f.token()).collect();
    assert_eq!(
        tokens,
        [
            "fail_closed_policy",
            "unparseable_shell",
            "opaque_shell",
            "exec_or_ambient_git",
            "env_injection",
            "unvetted_env",
            "file_write",
            "dangerous_command",
            "special_exec_surface",
        ]
    );
    let mut sorted = tokens.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), tokens.len(), "tokens must be unique");
}

#[test]
fn default_assessment_is_empty() {
    let a = BashSecurityAssessment::default();
    assert!(a.is_empty());
    assert!(!a.constrains_broad_grant());
    assert_eq!(a.render_tokens(), "[]");
    assert_eq!(a.render_glossary(), "");
}

#[test]
fn descriptions_are_fixed_per_variant() {
    let pairs = [
        (
            FailClosedPolicy,
            "permission policy could not determine whether a rule applies",
        ),
        (
            UnparseableShell,
            "shell structure could not be fully parsed",
        ),
        (
            OpaqueShell,
            "invokes a nested or dynamically supplied shell command",
        ),
        (
            ExecOrAmbientGit,
            "may run code via an execution callback or ambient Git configuration",
        ),
        (
            EnvInjection,
            "sets environment variables that can change which code executes",
        ),
        (
            UnvettedEnv,
            "sets environment variables outside the known-safe set",
        ),
        (FileWrite, "writes to a real file rather than a sink"),
        (DangerousCommand, "a destructive or high-impact command"),
        (
            SpecialExecSurface,
            "options or tools that can write arbitrary output, execute code, override config/identity, or disclose the process environment",
        ),
    ];
    for (finding, description) in pairs {
        assert_eq!(finding.description(), description, "{finding:?}");
    }
}

#[test]
fn assessment_is_ordered_and_deduplicated() {
    // Insert out of order, with duplicates; the BTreeSet invariant canonicalizes.
    let mut a = BashSecurityAssessment::default();
    a.insert(FileWrite);
    a.insert(OpaqueShell);
    a.insert(FileWrite);
    a.insert(FailClosedPolicy);
    a.insert(OpaqueShell);
    assert!(!a.is_empty());
    // Render order follows the enum discriminant order, not insertion order.
    assert_eq!(
        a.render_tokens(),
        "[fail_closed_policy, opaque_shell, file_write]"
    );
    // Glossary renders only present findings, in canonical order.
    assert_eq!(
        a.render_glossary(),
        "- fail_closed_policy: permission policy could not determine whether a rule applies\n\
         - opaque_shell: invokes a nested or dynamically supplied shell command\n\
         - file_write: writes to a real file rather than a sink"
    );
    assert!(a.contains(FileWrite));
    assert!(!a.contains(DangerousCommand));
}

#[test]
fn grant_floor_subset_is_exactly_the_broad_grant_findings() {
    // Findings that must block a broad grant / sandbox auto-allow.
    for f in [
        FileWrite,
        UnvettedEnv,
        EnvInjection,
        OpaqueShell,
        ExecOrAmbientGit,
        SpecialExecSurface,
    ] {
        let a: BashSecurityAssessment = [f].into_iter().collect();
        assert!(
            a.constrains_broad_grant(),
            "{f:?} must constrain a broad grant"
        );
    }
    // Findings the broad-grant path handles via their own decision arms.
    for f in [DangerousCommand, UnparseableShell, FailClosedPolicy] {
        let a: BashSecurityAssessment = [f].into_iter().collect();
        assert!(
            !a.constrains_broad_grant(),
            "{f:?} must not be a broad-grant floor (handled elsewhere)"
        );
    }
}
