//! Generic startup warnings displayed on the welcome screen.
//!
//! Any subsystem (terminal diagnostics, auth, config migration, etc.) can
//! produce [`StartupWarning`]s.

pub(crate) const DOCTOR_ACTION: &str = "Run /doctor for details and fixes.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionableStartupWarning {
    warning: StartupWarning,
    ids: Vec<crate::diagnostics::DiagnosticId>,
}

impl ActionableStartupWarning {
    pub(crate) fn new(
        severity: WarningSeverity,
        message: impl Into<String>,
        ids: impl IntoIterator<Item = crate::diagnostics::DiagnosticId>,
    ) -> Self {
        let ids = ids.into_iter().collect::<Vec<_>>();
        assert!(
            !ids.is_empty(),
            "doctor-linked startup notice requires an ID"
        );
        Self {
            warning: StartupWarning {
                severity,
                message: message.into(),
                action: Some(DOCTOR_ACTION.to_owned()),
            },
            ids,
        }
    }

    #[cfg(test)]
    pub(crate) fn ids(&self) -> &[crate::diagnostics::DiagnosticId] {
        &self.ids
    }

    pub(crate) fn into_warning(self) -> StartupWarning {
        self.warning
    }
}

/// A non-fatal startup warning from any subsystem.
///
/// This is a **display contract only** -- the subsystem formats the message
/// and optional action hint. Actionable diagnostic notices link to `/doctor`,
/// which owns detailed evidence and remediation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupWarning {
    /// Severity controls rendering color (yellow for warnings, dim for info).
    pub severity: WarningSeverity,
    /// Short, user-facing message (fits in ~60 columns).
    pub message: String,
    /// Optional action hint (e.g. "Run /doctor for details and fixes.").
    pub action: Option<String>,
}

/// Severity level for startup warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningSeverity {
    /// Rendered in warning color (yellow). Something is misconfigured.
    Warning,
    /// Rendered in dim/gray. Informational, not actionable.
    Info,
}

/// Pick the warning the single-slot welcome banner shows: the first
/// `Warning`-severity entry, else the last entry.
///
/// `startup_warnings` is appended to at runtime while the user sits on the
/// welcome screen (session-start failures, Claude import results), so a plain
/// `first()` lets an early entry mask that later feedback — e.g. an import
/// Info result at index 0 hides a session-start Warning pushed behind it.
/// Severity decides first (a real Warning always beats an Info; among
/// Warnings, assemble order stays authoritative); a Warning-less list falls
/// back to the **last** entry because later Info pushes are direct
/// user-action feedback that must not be masked by an older Info. Every
/// banner surface (height calc + render) must pick through here so they
/// cannot disagree.
pub fn banner_warning(warnings: &[StartupWarning]) -> Option<&StartupWarning> {
    warnings
        .iter()
        .find(|w| w.severity == WarningSeverity::Warning)
        .or_else(|| warnings.last())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(severity: WarningSeverity, message: &str) -> StartupWarning {
        StartupWarning {
            severity,
            message: message.to_owned(),
            action: None,
        }
    }

    #[test]
    fn banner_warning_empty_is_none() {
        assert!(banner_warning(&[]).is_none());
    }

    #[test]
    fn banner_warning_lone_info_shows() {
        let list = [entry(WarningSeverity::Info, "info note")];
        assert_eq!(banner_warning(&list).unwrap().message, "info note");
    }

    #[test]
    fn banner_warning_runtime_pushed_warning_displaces_info() {
        // An Info entry holds index 0 (e.g. a Claude import result). A
        // Warning pushed later (e.g. "Not inside a git repository") must
        // still win the single banner slot.
        let list = [
            entry(WarningSeverity::Info, "info note"),
            entry(WarningSeverity::Warning, "real problem"),
        ];
        assert_eq!(banner_warning(&list).unwrap().message, "real problem");
    }

    #[test]
    fn banner_warning_runtime_pushed_info_displaces_earlier_info() {
        // Warning-less list: a later Info push is direct user-action
        // feedback (e.g. a Claude import result) and wins over an older Info.
        let list = [
            entry(WarningSeverity::Info, "info note"),
            entry(WarningSeverity::Info, "import result"),
        ];
        assert_eq!(banner_warning(&list).unwrap().message, "import result");
    }

    #[test]
    fn banner_warning_first_warning_wins_among_warnings() {
        let list = [
            entry(WarningSeverity::Warning, "first"),
            entry(WarningSeverity::Warning, "second"),
            entry(WarningSeverity::Info, "info note"),
        ];
        assert_eq!(banner_warning(&list).unwrap().message, "first");
    }
}
