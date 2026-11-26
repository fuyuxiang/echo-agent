//! First-call-wins process identity labels carried on every product event.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::EnumCount, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum Entrypoint {
    /// Agent inside the interactive client, or the dedicated stdio agent.
    Embedded,
    /// Shared leader agent process serving many sessions.
    Leader,
    /// Interactive client process whose agent lives in a leader.
    Pager,
    /// One-shot command.
    Cli,
    /// Headless agent session, no TUI (scripts, CI, SDK harnesses).
    Headless,
    /// Remote agent server process.
    Workspace,
}

impl Entrypoint {
    pub(crate) const ALL: [Entrypoint; 6] = [
        Entrypoint::Embedded,
        Entrypoint::Leader,
        Entrypoint::Pager,
        Entrypoint::Cli,
        Entrypoint::Headless,
        Entrypoint::Workspace,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        self.into()
    }
}

const _: () = assert!(Entrypoint::ALL.len() == <Entrypoint as strum::EnumCount>::COUNT);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, strum::EnumCount, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Alpha,
    #[default]
    Unknown,
}

impl ReleaseChannel {
    pub(crate) const ALL: [ReleaseChannel; 3] = [
        ReleaseChannel::Stable,
        ReleaseChannel::Alpha,
        ReleaseChannel::Unknown,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        self.into()
    }

    pub fn from_label(label: &str) -> ReleaseChannel {
        match label.trim().trim_start_matches('[').trim_end_matches(']') {
            "stable" => ReleaseChannel::Stable,
            "alpha" => ReleaseChannel::Alpha,
            _ => ReleaseChannel::Unknown,
        }
    }
}

const _: () = assert!(ReleaseChannel::ALL.len() == <ReleaseChannel as strum::EnumCount>::COUNT);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaderMode {
    Attached,
    Standalone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interactivity {
    Interactive,
    Unattended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub entrypoint: Entrypoint,
    pub leader: LeaderMode,
    pub interactivity: Interactivity,
}

static IDENTITY: OnceLock<ProcessIdentity> = OnceLock::new();

pub fn set_identity(identity: ProcessIdentity) {
    let _ = IDENTITY.set(identity);
}

pub(crate) fn identity() -> Option<ProcessIdentity> {
    IDENTITY.get().copied()
}

pub(crate) fn entrypoint() -> Option<Entrypoint> {
    identity().map(|i| i.entrypoint)
}

static RELEASE_CHANNEL: OnceLock<ReleaseChannel> = OnceLock::new();

/// The updater owns the channel truth but depends on this crate, so entry
/// points pass the channel in.
pub fn set_release_channel(channel: ReleaseChannel) {
    if channel == ReleaseChannel::Unknown {
        return;
    }
    let _ = RELEASE_CHANNEL.set(channel);
}

pub(crate) fn release_channel() -> Option<ReleaseChannel> {
    RELEASE_CHANNEL.get().copied()
}

#[cfg(test)]
#[path = "process_info_tests.rs"]
mod tests;
