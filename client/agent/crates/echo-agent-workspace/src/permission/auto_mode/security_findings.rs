//! Typed, harness-owned security findings for the Auto-mode classifier.
//!
//! A finding is a fixed enum token naming a static-analysis risk the built-in
//! Bash gate detected, plus a fixed harness-owned description. Findings are
//! trusted harness facts, not untrusted data: token and description are static
//! constants that never carry a command, path, or argument (those stay in the
//! untrusted proposed action). An attacker therefore cannot steer the classifier
//! by smuggling text through a finding. Nonempty
//! findings force the model path — the heuristic pre-pass may not auto-Allow — so
//! the classifier always sees them before it can approve the action.
//!
//! [`BashSecurityAssessment`] is the single canonical finding set for one
//! request: it is built once at the detector sites, owns the ordered/deduplicated
//! invariant (a `BTreeSet`), and is the sole source for both broad-grant/sandbox
//! bypass prevention (`constrains_broad_grant`) and the classifier evidence.

use std::collections::BTreeSet;

/// One built-in Bash security finding. Closed set; each variant renders to a
/// single stable wire token. `Ord` fixes the canonical render order (variant
/// order below), so [`BashSecurityAssessment`] renders deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClassifierSecurityFinding {
    /// Managed-policy analysis failed closed with no explicit rule match.
    FailClosedPolicy,
    /// Shell structure the parser could not decompose to check.
    UnparseableShell,
    /// Nested/opaque shell execution (`bash -c "$X"`, `eval …`).
    OpaqueShell,
    /// Execution callback or ambient Git-config exec risk.
    ExecOrAmbientGit,
    /// Environment injection key (LD_PRELOAD, DYLD_*, GIT_CONFIG*, …).
    EnvInjection,
    /// Environment assignment outside the cosmetic-safe allowlist.
    UnvettedEnv,
    /// Write to a real (non-sink) file.
    FileWrite,
    /// Dangerous command segment (`rm`, `chmod`, `git push`, …).
    DangerousCommand,
    /// Special executable/disclosure surface (`rg --pre`, Git drivers/pagers,
    /// kubectl config/auth overrides, process-environment dump).
    SpecialExecSurface,
}

impl ClassifierSecurityFinding {
    /// Every finding variant, in canonical order. The single owner-side source of
    /// truth for the finding vocabulary, so cross-crate consumers (telemetry
    /// tokens) can assert coverage against it rather than a hand-copied list.
    pub const ALL: &'static [Self] = &[
        Self::FailClosedPolicy,
        Self::UnparseableShell,
        Self::OpaqueShell,
        Self::ExecOrAmbientGit,
        Self::EnvInjection,
        Self::UnvettedEnv,
        Self::FileWrite,
        Self::DangerousCommand,
        Self::SpecialExecSurface,
    ];

    /// Stable wire token rendered into the classifier system message. Never
    /// changes without a matching classifier-prompt update.
    pub const fn token(self) -> &'static str {
        match self {
            Self::FailClosedPolicy => "fail_closed_policy",
            Self::UnparseableShell => "unparseable_shell",
            Self::OpaqueShell => "opaque_shell",
            Self::ExecOrAmbientGit => "exec_or_ambient_git",
            Self::EnvInjection => "env_injection",
            Self::UnvettedEnv => "unvetted_env",
            Self::FileWrite => "file_write",
            Self::DangerousCommand => "dangerous_command",
            Self::SpecialExecSurface => "special_exec_surface",
        }
    }

    /// Fixed, harness-owned explanation of the token. Like [`token`](Self::token)
    /// it is a static string carrying no command, path, argument, or user text,
    /// so it cannot be used to inject instructions.
    pub const fn description(self) -> &'static str {
        match self {
            Self::FailClosedPolicy => {
                "permission policy could not determine whether a rule applies"
            }
            Self::UnparseableShell => "shell structure could not be fully parsed",
            Self::OpaqueShell => "invokes a nested or dynamically supplied shell command",
            Self::ExecOrAmbientGit => {
                "may run code via an execution callback or ambient Git configuration"
            }
            Self::EnvInjection => "sets environment variables that can change which code executes",
            Self::UnvettedEnv => "sets environment variables outside the known-safe set",
            Self::FileWrite => "writes to a real file rather than a sink",
            Self::DangerousCommand => "a destructive or high-impact command",
            Self::SpecialExecSurface => {
                "options or tools that can write arbitrary output, execute code, override config/identity, or disclose the process environment"
            }
        }
    }

    /// Whether this finding constrains a *broad* grant (session `allow_bash_execute`
    /// blanket, prefix/glob grant, sandbox auto-allow). These are effects a broad
    /// grant cannot vouch for, so their presence must reach the classifier rather
    /// than auto-allow. `DangerousCommand` is excluded here because the blanket
    /// grant path already refuses dangerous segments; `UnparseableShell` /
    /// `FailClosedPolicy` are handled by their own decision arms.
    const fn is_grant_floor(self) -> bool {
        matches!(
            self,
            Self::FileWrite
                | Self::UnvettedEnv
                | Self::EnvInjection
                | Self::OpaqueShell
                | Self::ExecOrAmbientGit
                | Self::SpecialExecSurface
        )
    }
}

/// The single canonical, ordered, deduplicated finding set for one request.
///
/// The `BTreeSet` encodes the ordering/dedup invariant in the type itself, so no
/// caller can supply duplicates or arbitrary order. Built once at the detector
/// sites in `evaluate_bash`; the manager clones it and adds the managed-policy
/// `FailClosedPolicy` finding when a gate failed closed. `Default` is the empty
/// assessment (non-Bash access, or a fully safe command).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BashSecurityAssessment(BTreeSet<ClassifierSecurityFinding>);

impl BashSecurityAssessment {
    /// Record a detected finding (idempotent; order-independent).
    pub(crate) fn insert(&mut self, finding: ClassifierSecurityFinding) {
        self.0.insert(finding);
    }

    /// No findings: the command is fully safe by static analysis. A broad
    /// policy Allow / fast path may only skip the classifier when this holds.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the given finding is present (readable across the crate boundary
    /// for callers that key behavior on one specific finding, e.g. tests).
    pub fn contains(&self, finding: ClassifierSecurityFinding) -> bool {
        self.0.contains(&finding)
    }

    /// Whether any finding constrains a broad grant / sandbox auto-allow — the
    /// single source for `bash_request_floor_requires_prompt`.
    pub(crate) fn constrains_broad_grant(&self) -> bool {
        self.0
            .iter()
            .copied()
            .any(ClassifierSecurityFinding::is_grant_floor)
    }

    /// Whether `FileWrite` is the only finding — the one floor a narrow allow
    /// rule naming a write-capable command can vouch for; mixed assessments
    /// never qualify.
    pub(crate) fn is_file_write_only(&self) -> bool {
        self.0.len() == 1 && self.0.contains(&ClassifierSecurityFinding::FileWrite)
    }

    /// Compact `[token, token]` list in canonical order, for tests to pin the
    /// ordered/deduplicated invariant. The system message uses `render_glossary`.
    #[cfg(test)]
    pub(crate) fn render_tokens(&self) -> String {
        let tokens: Vec<&str> = self.0.iter().map(|f| f.token()).collect();
        format!("[{}]", tokens.join(", "))
    }

    /// Stable, ordered, deduplicated wire tokens for one request — the single
    /// projection consumed by manager telemetry and trace evidence. Findings are
    /// never recomputed for analytics; this is the canonical set handed to the
    /// classifier. Crate-private so the finding set never crosses the crate
    /// boundary (external OTEL must not gain finding fields).
    pub(crate) fn tokens(&self) -> Vec<String> {
        self.0.iter().map(|f| f.token().to_owned()).collect()
    }

    /// One `- token: description` line per present finding, in canonical order.
    pub(crate) fn render_glossary(&self) -> String {
        self.0
            .iter()
            .map(|f| format!("- {}: {}", f.token(), f.description()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl FromIterator<ClassifierSecurityFinding> for BashSecurityAssessment {
    fn from_iter<I: IntoIterator<Item = ClassifierSecurityFinding>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
#[path = "security_findings_tests.rs"]
mod tests;
