//! Environment-based delivery and toast policy for clipboard writes.
//!
//! Writes still multi-fire every backend; this module classifies whether a
//! successful leg is known to reach the destination named by the UI.

use crate::host::{DisplayServer, HostOs};
use crate::terminal::TerminalName;

use super::{ClipboardFeedback, ClipboardWriteLegs};

/// EchoAgent's evidence that a clipboard write reached its intended destination.
#[derive(Debug, Clone, Copy, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ClipboardDelivery {
    /// A successful write leg has a destination trusted by the environment policy.
    Confirmed,
    /// OSC 52 was emitted, but the outer terminal's clipboard support is unknown.
    Unverified,
    /// No usable write leg succeeded, or the destination is known not to support it.
    Failed,
}

impl ClipboardDelivery {
    pub fn is_confirmed(self) -> bool {
        self == Self::Confirmed
    }

    pub fn is_failed(self) -> bool {
        self == Self::Failed
    }

    pub fn reported_success(self) -> bool {
        matches!(self, Self::Confirmed | Self::Unverified)
    }

    pub fn telemetry_label(self) -> &'static str {
        self.into()
    }
}

/// Clipboard-relevant facts about the terminal and host environment.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[doc(hidden)]
pub struct ClipboardEnvironment {
    pub brand: TerminalName,
    pub host_os: HostOs,
    pub display_server: DisplayServer,
    pub remote: bool,
    pub container: bool,
    pub osc52_sink: bool,
    pub wayland_data_control: bool,
    pub wl_copy_available: bool,
}

/// The terminal's advertised OSC 52 clipboard capability.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[doc(hidden)]
pub enum Osc52Capability {
    Supported,
    Unsupported,
    Unknown,
}

impl Osc52Capability {
    #[doc(hidden)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

impl ClipboardEnvironment {
    #[doc(hidden)]
    pub fn osc52_capability(self) -> Osc52Capability {
        if self.osc52_sink || self.brand.supports_osc52_clipboard() {
            Osc52Capability::Supported
        } else if self.brand == TerminalName::Unknown {
            Osc52Capability::Unknown
        } else {
            Osc52Capability::Unsupported
        }
    }
}

/// Native clipboard route evidence available before a copy is attempted.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NativeClipboardPreflight {
    Disabled,
    LocalAvailable,
    RemoteOnly,
    Unavailable,
}

fn trusted_wayland_native(wl_copy: bool, arboard: bool, data_control: bool) -> bool {
    wl_copy || (arboard && data_control)
}

/// Classify the configured native route without claiming that a write succeeded.
pub fn native_clipboard_preflight(
    route_native: bool,
    environment: ClipboardEnvironment,
) -> NativeClipboardPreflight {
    if !route_native {
        return NativeClipboardPreflight::Disabled;
    }
    if environment.remote || environment.container {
        return NativeClipboardPreflight::RemoteOnly;
    }
    match environment.host_os {
        HostOs::Linux => match environment.display_server {
            DisplayServer::Wayland
                if environment.wl_copy_available || environment.wayland_data_control =>
            {
                NativeClipboardPreflight::LocalAvailable
            }
            DisplayServer::Wayland | DisplayServer::Unknown => {
                NativeClipboardPreflight::Unavailable
            }
            DisplayServer::X11 => NativeClipboardPreflight::LocalAvailable,
            DisplayServer::Quartz | DisplayServer::Win32 => NativeClipboardPreflight::Unavailable,
        },
        HostOs::Macos | HostOs::Windows => NativeClipboardPreflight::LocalAvailable,
        HostOs::Other => NativeClipboardPreflight::Unavailable,
    }
}

/// Classify one emitted OSC 52 write.
/// Unknown SSH/container boundaries strip brand markers, so missing capability evidence is Unverified rather than Failed.
pub(crate) fn osc52_delivery(environment: ClipboardEnvironment) -> ClipboardDelivery {
    match environment.osc52_capability() {
        Osc52Capability::Supported => ClipboardDelivery::Confirmed,
        Osc52Capability::Unknown if environment.remote || environment.container => {
            ClipboardDelivery::Unverified
        }
        Osc52Capability::Unknown | Osc52Capability::Unsupported => ClipboardDelivery::Failed,
    }
}

/// Expected preflight confidence for an enabled clipboard route.
pub fn expected_delivery(
    native: NativeClipboardPreflight,
    route_tmux: bool,
    route_osc52: bool,
    environment: ClipboardEnvironment,
) -> ClipboardDelivery {
    if native == NativeClipboardPreflight::LocalAvailable {
        return ClipboardDelivery::Confirmed;
    }
    let osc52 = route_osc52.then(|| osc52_delivery(environment));
    if osc52 == Some(ClipboardDelivery::Confirmed) || route_tmux {
        return ClipboardDelivery::Confirmed;
    }
    if osc52 == Some(ClipboardDelivery::Unverified) {
        return ClipboardDelivery::Unverified;
    }
    ClipboardDelivery::Failed
}

/// True when native legs wrote the local OS clipboard rather than a remote host.
pub(crate) fn trusted_native(legs: &ClipboardWriteLegs, environment: ClipboardEnvironment) -> bool {
    if environment.remote || environment.container || !legs.route_native {
        return false;
    }
    match environment.host_os {
        HostOs::Linux => match environment.display_server {
            DisplayServer::Wayland => {
                trusted_wayland_native(legs.wl_copy_ok, legs.arboard_ok, legs.data_control)
            }
            _ => legs.cli_ok || legs.arboard_ok,
        },
        HostOs::Macos | HostOs::Windows | HostOs::Other => legs.cli_ok || legs.arboard_ok,
    }
}

/// Resolve the user-visible feedback; each feedback variant owns its delivery state.
pub(crate) fn resolve_copy_decision(
    legs: &ClipboardWriteLegs,
    text: &str,
    environment: ClipboardEnvironment,
) -> ClipboardFeedback {
    if trusted_native(legs, environment) {
        return ClipboardFeedback::Copied;
    }
    if legs.osc52_ok {
        match osc52_delivery(environment) {
            ClipboardDelivery::Confirmed => {
                if environment.container {
                    return ClipboardFeedback::CopiedOscContainer;
                }
                if environment.remote && environment.brand.is_vscode_family() && !text.is_ascii() {
                    return ClipboardFeedback::VsCodeSshNonAscii;
                }
                if environment.remote {
                    return ClipboardFeedback::CopiedOscRemote;
                }
                return ClipboardFeedback::Copied;
            }
            ClipboardDelivery::Unverified if !legs.tmux_ok => {
                if environment.container {
                    return ClipboardFeedback::UnverifiedOscContainer;
                }
                return ClipboardFeedback::UnverifiedOscRemote;
            }
            ClipboardDelivery::Unverified | ClipboardDelivery::Failed => {}
        }
    }
    if legs.tmux_ok {
        return ClipboardFeedback::CopiedTmux;
    }
    if environment.remote || environment.container {
        ClipboardFeedback::FailedRemote
    } else {
        ClipboardFeedback::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legs(
        cli_ok: bool,
        arboard_ok: bool,
        data_control: bool,
        tmux_ok: bool,
        osc52_ok: bool,
        cli_ok_tools: &str,
    ) -> ClipboardWriteLegs {
        ClipboardWriteLegs {
            route_native: true,
            route_label: "test".into(),
            cli_tools_tried: String::new(),
            cli_ok_tools: cli_ok_tools.into(),
            wl_copy_ok: cli_ok_tools.split('+').any(|tool| tool == "wl-copy"),
            cli_ok,
            arboard_ok,
            data_control,
            tmux_ok,
            osc52_ok,
        }
    }

    fn environment(brand: TerminalName) -> ClipboardEnvironment {
        ClipboardEnvironment {
            brand,
            host_os: HostOs::Linux,
            display_server: DisplayServer::Unknown,
            remote: false,
            container: false,
            osc52_sink: false,
            wayland_data_control: false,
            wl_copy_available: false,
        }
    }

    #[test]
    fn telemetry_projection_labels_and_historical_boolean_are_pinned() {
        for (delivery, label, confirmed, failed, reported_success) in [
            (ClipboardDelivery::Confirmed, "confirmed", true, false, true),
            (
                ClipboardDelivery::Unverified,
                "unverified",
                false,
                false,
                true,
            ),
            (ClipboardDelivery::Failed, "failed", false, true, false),
        ] {
            assert_eq!(delivery.telemetry_label(), label);
            assert_eq!(delivery.is_confirmed(), confirmed);
            assert_eq!(delivery.is_failed(), failed);
            assert_eq!(delivery.reported_success(), reported_success);
        }
    }

    #[test]
    fn local_trusted_native_is_confirmed() {
        let feedback = resolve_copy_decision(
            &legs(true, false, false, false, false, "pbcopy"),
            "hello",
            ClipboardEnvironment {
                host_os: HostOs::Macos,
                display_server: DisplayServer::Quartz,
                ..environment(TerminalName::Ghostty)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::Copied);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Confirmed);
    }

    #[test]
    fn wayland_native_requires_verified_destination() {
        let environment = ClipboardEnvironment {
            display_server: DisplayServer::Wayland,
            ..environment(TerminalName::Vte)
        };
        let unverified = legs(false, true, false, false, false, "");
        assert!(!trusted_native(&unverified, environment));
        let data_control = legs(false, true, true, false, false, "");
        assert!(trusted_native(&data_control, environment));
        let wl_copy = legs(true, false, false, false, false, "wl-copy");
        assert!(trusted_native(&wl_copy, environment));
    }

    #[test]
    fn remote_native_write_only_uses_failed_remote() {
        let feedback = resolve_copy_decision(
            &legs(true, true, false, false, false, "xclip"),
            "hello",
            ClipboardEnvironment {
                display_server: DisplayServer::X11,
                remote: true,
                ..environment(TerminalName::Ghostty)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::FailedRemote);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Failed);
    }

    #[test]
    fn known_osc_capable_terminal_is_confirmed() {
        let feedback = resolve_copy_decision(
            &legs(false, false, false, false, true, ""),
            "hello",
            ClipboardEnvironment {
                remote: true,
                ..environment(TerminalName::Ghostty)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::CopiedOscRemote);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Confirmed);
    }

    #[test]
    fn ssh_unknown_brand_osc_is_unverified() {
        let feedback = resolve_copy_decision(
            &legs(false, false, false, false, true, ""),
            "hello",
            ClipboardEnvironment {
                remote: true,
                ..environment(TerminalName::Unknown)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::UnverifiedOscRemote);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Unverified);
    }

    #[test]
    fn container_unknown_brand_osc_is_unverified() {
        let feedback = resolve_copy_decision(
            &legs(false, false, false, false, true, ""),
            "hello",
            ClipboardEnvironment {
                container: true,
                ..environment(TerminalName::Unknown)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::UnverifiedOscContainer);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Unverified);
    }

    #[test]
    fn known_unsupported_terminal_osc_is_failed_remote() {
        for (brand, remote, container) in [
            (TerminalName::AppleTerminal, true, false),
            (TerminalName::Vte, true, false),
            (TerminalName::AppleTerminal, true, true),
        ] {
            let feedback = resolve_copy_decision(
                &legs(false, false, false, false, true, ""),
                "hello",
                ClipboardEnvironment {
                    remote,
                    container,
                    ..environment(brand)
                },
            );
            assert_eq!(feedback, ClipboardFeedback::FailedRemote, "{brand:?}");
            assert_eq!(feedback.delivery(), ClipboardDelivery::Failed, "{brand:?}");
        }
    }

    #[test]
    fn container_with_detected_unsupported_brand_is_failed_remote() {
        let feedback = resolve_copy_decision(
            &legs(false, false, false, false, true, ""),
            "hello",
            ClipboardEnvironment {
                container: true,
                ..environment(TerminalName::AppleTerminal)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::FailedRemote);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Failed);
    }

    #[test]
    fn active_wrap_sink_with_osc_is_confirmed_for_any_brand() {
        for brand in [TerminalName::Unknown, TerminalName::AppleTerminal] {
            let feedback = resolve_copy_decision(
                &legs(false, false, false, false, true, ""),
                "hello",
                ClipboardEnvironment {
                    remote: true,
                    osc52_sink: true,
                    ..environment(brand)
                },
            );
            assert!(feedback.delivery().is_confirmed(), "{brand:?}");
        }
    }

    #[test]
    fn wrap_sink_without_osc_write_is_failed_remote() {
        let feedback = resolve_copy_decision(
            &legs(false, false, false, false, false, ""),
            "hello",
            ClipboardEnvironment {
                remote: true,
                osc52_sink: true,
                ..environment(TerminalName::Unknown)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::FailedRemote);
    }

    #[test]
    fn tmux_success_wins_over_unverified_osc() {
        let feedback = resolve_copy_decision(
            &legs(false, false, false, true, true, ""),
            "hello",
            ClipboardEnvironment {
                remote: true,
                ..environment(TerminalName::Unknown)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::CopiedTmux);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Confirmed);
    }

    #[test]
    fn no_successful_local_leg_is_failed() {
        let feedback = resolve_copy_decision(
            &legs(false, false, false, false, false, ""),
            "hello",
            environment(TerminalName::Ghostty),
        );
        assert_eq!(feedback, ClipboardFeedback::Failed);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Failed);
    }

    #[test]
    fn remote_and_container_prefer_container_feedback_and_telemetry_branch() {
        let confirmed = resolve_copy_decision(
            &legs(false, false, false, false, true, ""),
            "hello",
            ClipboardEnvironment {
                remote: true,
                container: true,
                ..environment(TerminalName::Ghostty)
            },
        );
        assert_eq!(confirmed, ClipboardFeedback::CopiedOscContainer);
        assert_eq!(
            Into::<&'static str>::into(confirmed),
            "copied_osc_container"
        );

        let unverified = resolve_copy_decision(
            &legs(false, false, false, false, true, ""),
            "hello",
            ClipboardEnvironment {
                remote: true,
                container: true,
                ..environment(TerminalName::Unknown)
            },
        );
        assert_eq!(unverified, ClipboardFeedback::UnverifiedOscContainer);
        assert_eq!(
            Into::<&'static str>::into(unverified),
            "unverified_osc_container"
        );
    }

    #[test]
    fn vscode_ssh_non_ascii_stays_confirmed_with_warning_toast() {
        let feedback = resolve_copy_decision(
            &legs(false, false, false, false, true, ""),
            "café",
            ClipboardEnvironment {
                remote: true,
                ..environment(TerminalName::VsCode)
            },
        );
        assert_eq!(feedback, ClipboardFeedback::VsCodeSshNonAscii);
        assert_eq!(feedback.delivery(), ClipboardDelivery::Confirmed);
    }

    #[test]
    fn native_preflight_matches_observed_wayland_trust_matrix() {
        for (data_control, wl_copy, expected) in [
            (false, false, NativeClipboardPreflight::Unavailable),
            (false, true, NativeClipboardPreflight::LocalAvailable),
            (true, false, NativeClipboardPreflight::LocalAvailable),
            (true, true, NativeClipboardPreflight::LocalAvailable),
        ] {
            assert_eq!(
                native_clipboard_preflight(
                    true,
                    ClipboardEnvironment {
                        display_server: DisplayServer::Wayland,
                        wayland_data_control: data_control,
                        wl_copy_available: wl_copy,
                        ..environment(TerminalName::Vte)
                    },
                ),
                expected,
                "data_control={data_control} wl_copy={wl_copy}"
            );
        }
        for (remote, container) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                native_clipboard_preflight(
                    true,
                    ClipboardEnvironment {
                        display_server: DisplayServer::Wayland,
                        remote,
                        container,
                        wayland_data_control: true,
                        wl_copy_available: true,
                        ..environment(TerminalName::Vte)
                    },
                ),
                NativeClipboardPreflight::RemoteOnly,
                "remote={remote} container={container}"
            );
        }
    }

    #[test]
    fn expected_delivery_matches_preflight_routes() {
        let unknown_remote = ClipboardEnvironment {
            remote: true,
            ..environment(TerminalName::Unknown)
        };
        assert_eq!(
            expected_delivery(
                NativeClipboardPreflight::RemoteOnly,
                false,
                true,
                unknown_remote,
            ),
            ClipboardDelivery::Unverified
        );
        assert_eq!(
            expected_delivery(
                NativeClipboardPreflight::RemoteOnly,
                false,
                true,
                ClipboardEnvironment {
                    remote: true,
                    ..environment(TerminalName::Vte)
                },
            ),
            ClipboardDelivery::Failed
        );
        assert_eq!(
            expected_delivery(
                NativeClipboardPreflight::RemoteOnly,
                false,
                true,
                ClipboardEnvironment {
                    remote: true,
                    osc52_sink: true,
                    ..environment(TerminalName::Vte)
                },
            ),
            ClipboardDelivery::Confirmed
        );
        assert_eq!(
            expected_delivery(
                NativeClipboardPreflight::RemoteOnly,
                true,
                false,
                unknown_remote,
            ),
            ClipboardDelivery::Confirmed
        );
        assert_eq!(
            expected_delivery(
                NativeClipboardPreflight::Unavailable,
                false,
                false,
                environment(TerminalName::Vte),
            ),
            ClipboardDelivery::Failed
        );
    }
}
