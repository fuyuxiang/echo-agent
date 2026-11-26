use super::{
    Entrypoint, Interactivity, LeaderMode, ProcessIdentity, ReleaseChannel, entrypoint, identity,
    release_channel, set_identity, set_release_channel,
};

#[test]
fn the_first_recorded_identity_wins_whole_and_wire_values_are_stable() {
    let first = ProcessIdentity {
        entrypoint: Entrypoint::Cli,
        leader: LeaderMode::Standalone,
        interactivity: Interactivity::Unattended,
    };
    set_identity(first);
    set_identity(ProcessIdentity {
        entrypoint: Entrypoint::Leader,
        leader: LeaderMode::Attached,
        interactivity: Interactivity::Interactive,
    });
    assert_eq!(identity(), Some(first));
    assert_eq!(entrypoint(), Some(Entrypoint::Cli));

    let labels: Vec<&str> = Entrypoint::ALL
        .iter()
        .map(|entrypoint| entrypoint.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "embedded",
            "leader",
            "pager",
            "cli",
            "headless",
            "workspace"
        ]
    );
}

#[test]
fn release_channel_labels_map_to_the_closed_set() {
    assert_eq!(
        ReleaseChannel::from_label(" [alpha]"),
        ReleaseChannel::Alpha
    );
    assert_eq!(
        ReleaseChannel::from_label(" [stable]"),
        ReleaseChannel::Stable
    );
    assert_eq!(ReleaseChannel::from_label("alpha"), ReleaseChannel::Alpha);
    assert_eq!(ReleaseChannel::from_label("stable"), ReleaseChannel::Stable);
    assert_eq!(ReleaseChannel::from_label(""), ReleaseChannel::Unknown);
    assert_eq!(ReleaseChannel::from_label("beta"), ReleaseChannel::Unknown);
    assert_eq!(
        ReleaseChannel::from_label(" [nightly]"),
        ReleaseChannel::Unknown
    );

    let labels: Vec<&str> = ReleaseChannel::ALL
        .iter()
        .map(|channel| channel.as_str())
        .collect();
    assert_eq!(labels, ["stable", "alpha", "unknown"]);
}

/// Only this test sets the process-global `RELEASE_CHANNEL` in this binary.
#[test]
fn setting_unknown_leaves_the_channel_unset() {
    set_release_channel(ReleaseChannel::Unknown);
    assert_eq!(release_channel(), None, "unknown records nothing");
    set_release_channel(ReleaseChannel::Alpha);
    assert_eq!(
        release_channel(),
        Some(ReleaseChannel::Alpha),
        "a later known channel still wins the slot"
    );
}
