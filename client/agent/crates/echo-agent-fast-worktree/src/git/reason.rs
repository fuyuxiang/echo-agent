#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Safety {
    Delete,
    Keep(KeepReason),
}

#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeepReason {
    Dirty,
    HiddenFromStatus,
    IgnoredContent(String),
    EmbeddedRepo(String),
    Unpushed,
    OnlyCopy,
    NotInSnapshot(String),
    WorktreeLocalState(String),
    NoRepo,
    CheckFailed,
    GateTimedOut,
}

impl KeepReason {
    fn detail(&self) -> Option<&str> {
        match self {
            KeepReason::IgnoredContent(what)
            | KeepReason::EmbeddedRepo(what)
            | KeepReason::WorktreeLocalState(what)
            | KeepReason::NotInSnapshot(what) => Some(what),
            KeepReason::Dirty
            | KeepReason::HiddenFromStatus
            | KeepReason::Unpushed
            | KeepReason::OnlyCopy
            | KeepReason::NoRepo
            | KeepReason::CheckFailed
            | KeepReason::GateTimedOut => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            KeepReason::Dirty => "dirty",
            KeepReason::HiddenFromStatus => "hidden-from-status",
            KeepReason::IgnoredContent(_) => "ignored-content",
            KeepReason::EmbeddedRepo(_) => "embedded-repo",
            KeepReason::Unpushed => "unpushed",
            KeepReason::OnlyCopy => "only-copy",
            KeepReason::NotInSnapshot(_) => "not-in-snapshot",
            KeepReason::WorktreeLocalState(_) => "worktree-local-state",
            KeepReason::NoRepo => "no-repo",
            KeepReason::CheckFailed => "check-failed",
            KeepReason::GateTimedOut => "gate-timed-out",
        }
    }
}

impl std::fmt::Display for KeepReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.detail() {
            Some(what) => write!(f, "{}: {what}", self.name()),
            None => f.write_str(self.name()),
        }
    }
}
