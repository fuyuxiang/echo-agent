//! Diagnostics view tests.

use super::*;
use crate::clipboard::ClipboardRoute;
use crate::diagnostics::probes::{
    ClipboardProbeFacts, TmuxProbeFacts, TuiProbeEvidence, WaylandProbeFacts,
};
use crate::terminal::{MultiplexerKind, TerminalContext, TerminalName};
use crate::theme::color_support::ColorLevel;

static ROUTE: ClipboardRoute = ClipboardRoute {
    native: true,
    tmux_buffer: true,
    osc52: true,
    osc52_tmux_passthrough: true,
};

fn snapshot<'a>(
    terminal: &'a TerminalContext,
    tmux: TmuxProbeFacts,
    runtime: DiagnosticRuntimeEvidence<'a>,
    osc52_sink_active: bool,
) -> DiagnosticSnapshot<'a> {
    snapshot_for_host(
        terminal,
        tmux,
        runtime,
        osc52_sink_active,
        crate::host::HostOs::Macos,
    )
}

fn snapshot_for_host<'a>(
    terminal: &'a TerminalContext,
    tmux: TmuxProbeFacts,
    runtime: DiagnosticRuntimeEvidence<'a>,
    osc52_sink_active: bool,
    host_os: crate::host::HostOs,
) -> DiagnosticSnapshot<'a> {
    snapshot_with_wayland_for_host(
        terminal,
        tmux,
        runtime,
        osc52_sink_active,
        WaylandProbeFacts {
            is_wayland: false,
            data_control: TmuxProbeResult::Unavailable,
            wl_copy_available: false,
        },
        host_os,
    )
}

fn snapshot_with_wayland<'a>(
    terminal: &'a TerminalContext,
    tmux: TmuxProbeFacts,
    runtime: DiagnosticRuntimeEvidence<'a>,
    osc52_sink_active: bool,
    wayland: WaylandProbeFacts,
) -> DiagnosticSnapshot<'a> {
    snapshot_with_wayland_for_host(
        terminal,
        tmux,
        runtime,
        osc52_sink_active,
        wayland,
        crate::host::HostOs::Macos,
    )
}

fn snapshot_with_wayland_for_host<'a>(
    terminal: &'a TerminalContext,
    tmux: TmuxProbeFacts,
    runtime: DiagnosticRuntimeEvidence<'a>,
    osc52_sink_active: bool,
    wayland: WaylandProbeFacts,
    host_os: crate::host::HostOs,
) -> DiagnosticSnapshot<'a> {
    DiagnosticSnapshot::from_parts(
        ProbeSnapshot {
            terminal,
            tmux,
            wayland,
            runtime: TuiProbeEvidence {
                fullscreen_active: false,
                kitty_flags_pushed: false,
                xtversion: match runtime.xtversion {
                    RuntimeEvidence::Available(xtversion) => xtversion,
                    RuntimeEvidence::Unavailable => None,
                },
            },
        },
        ClipboardProbeFacts {
            route: ROUTE.clone(),
            native_tool: "pbcopy",
            osc52_sink_active,
        },
        host_os,
        crate::host::DisplayServer::Unknown,
        false,
        ColorLevel::TrueColor,
        runtime,
    )
}

fn runtime(
    kitty_flags_pushed: RuntimeEvidence<bool>,
    xtversion: RuntimeEvidence<Option<&'static str>>,
) -> DiagnosticRuntimeEvidence<'static> {
    DiagnosticRuntimeEvidence {
        fullscreen_active: RuntimeEvidence::Available(true),
        kitty_flags_pushed,
        xtversion,
    }
}

fn available_runtime() -> DiagnosticRuntimeEvidence<'static> {
    runtime(
        RuntimeEvidence::Available(false),
        RuntimeEvidence::Available(None),
    )
}

#[test]
fn warning_category_ids_are_stable() {
    let ids = [
        WarningCategory::Clipboard,
        WarningCategory::DcsPassthrough,
        WarningCategory::ControlMode,
        WarningCategory::ByobuScreen,
        WarningCategory::UnsupportedTerminal,
        WarningCategory::TmuxExtendedKeysOff,
        WarningCategory::WaylandNoDataControl,
        WarningCategory::WezTermKittyKeyboardOff,
        WarningCategory::LimitedColorSupport,
        WarningCategory::SshWithoutWrap,
        WarningCategory::NotificationProtocolFallback,
        WarningCategory::FocusTrackingUnavailable,
        WarningCategory::SandboxProfileConflict,
    ]
    .map(|category| id_for(category).expect("diagnostic category must have an ID"))
    .map(|id| id.to_string());

    assert_eq!(
        ids,
        [
            "terminal.tmux-clipboard",
            "terminal.dcs-passthrough",
            "terminal.control-mode",
            "terminal.byobu-screen",
            "terminal.unsupported-emulator",
            "terminal.tmux-extended-keys",
            "terminal.wayland-data-control",
            "terminal.wezterm-kitty",
            "terminal.limited-color",
            "terminal.ssh-wrap",
            "notifications.protocol-fallback",
            "notifications.focus-tracking-unavailable",
            "sandbox.profile-conflict",
        ]
    );
}

#[test]
fn findings_have_stable_semantic_ids_and_dispositions() {
    let terminal = TerminalContext {
        brand: TerminalName::Iterm2,
        env_brand: TerminalName::Iterm2,
        multiplexer: MultiplexerKind::Tmux,
        is_ssh: true,
        ..Default::default()
    };
    let report = view(snapshot(
        &terminal,
        TmuxProbeFacts {
            version: TmuxProbeResult::Unavailable,
            extended_keys: TmuxProbeResult::Unavailable,
            set_clipboard: TmuxProbeResult::Available("off".to_owned()),
            allow_passthrough_support: TmuxProbeResult::Available(()),
            allow_passthrough: TmuxProbeResult::Available("on".to_owned()),
            control_mode: TmuxProbeResult::Available(false),
            client_features: TmuxProbeResult::Unavailable,
        },
        available_runtime(),
        false,
    ));

    assert_eq!(
        report
            .findings
            .iter()
            .map(|finding| (finding.id, finding.disposition))
            .collect::<Vec<_>>(),
        [
            (
                DiagnosticId::new("terminal", "tmux-clipboard"),
                FindingDisposition::Issue,
            ),
            (
                crate::diagnostics::ITERM2_CLIPBOARD_PERMISSION_ID,
                FindingDisposition::Recommendation,
            ),
            (
                DiagnosticId::new("terminal", "ssh-wrap"),
                FindingDisposition::Recommendation,
            ),
        ]
    );
    assert_eq!(
        report.facts.clipboard.delivery,
        crate::clipboard::ClipboardDelivery::Confirmed
    );
    assert_eq!(
        report.facts.clipboard.native_preflight,
        crate::clipboard::NativeClipboardPreflight::RemoteOnly
    );
    let ssh_wrap = report
        .findings
        .iter()
        .find(|finding| finding.id == crate::diagnostics::SSH_WRAP_ID)
        .expect("SSH wrap recommendation");
    assert_eq!(
        ssh_wrap.automatic_remediation,
        Some(crate::diagnostics::ssh_wrap_automatic_remediation())
    );
    assert_eq!(
        report.findings[0].automatic_remediation,
        crate::diagnostics::automatic_remediation_for(DiagnosticId::new(
            "terminal",
            "tmux-clipboard"
        ))
    );
}

#[test]
fn all_tmux_finding_metadata_uses_stable_automatic_fix_ids_without_schema_changes() {
    let mut terminal = TerminalContext {
        brand: TerminalName::Iterm2,
        env_brand: TerminalName::Iterm2,
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 3.4".to_owned()),
        tmux_extended_keys: Some("off".to_owned()),
        ..Default::default()
    };
    let report = view(snapshot(
        &terminal,
        TmuxProbeFacts {
            version: TmuxProbeResult::Available("tmux 3.4".to_owned()),
            extended_keys: TmuxProbeResult::Available("off".to_owned()),
            set_clipboard: TmuxProbeResult::Available("off".to_owned()),
            allow_passthrough_support: TmuxProbeResult::Available(()),
            allow_passthrough: TmuxProbeResult::Available("off".to_owned()),
            control_mode: TmuxProbeResult::Available(false),
            client_features: TmuxProbeResult::Unavailable,
        },
        available_runtime(),
        false,
    ));

    assert_eq!(
        report
            .findings
            .iter()
            .filter_map(|finding| finding.automatic_remediation)
            .map(|automatic| (automatic.fix_id, automatic.command))
            .collect::<Vec<_>>(),
        [
            (
                crate::diagnostics::TMUX_CLIPBOARD_ID,
                "echoagent doctor fix terminal.tmux-clipboard",
            ),
            (
                crate::diagnostics::DCS_PASSTHROUGH_ID,
                "echoagent doctor fix terminal.dcs-passthrough",
            ),
            (
                crate::diagnostics::TMUX_EXTENDED_KEYS_ID,
                "echoagent doctor fix terminal.tmux-extended-keys",
            ),
        ]
    );

    terminal.tmux_extended_keys = Some("on".to_owned());
    let healthy = view(snapshot(
        &terminal,
        TmuxProbeFacts {
            version: TmuxProbeResult::Available("tmux 3.4".to_owned()),
            extended_keys: TmuxProbeResult::Available("on".to_owned()),
            set_clipboard: TmuxProbeResult::Available("external".to_owned()),
            allow_passthrough_support: TmuxProbeResult::Available(()),
            allow_passthrough: TmuxProbeResult::Available("all".to_owned()),
            control_mode: TmuxProbeResult::Available(false),
            client_features: TmuxProbeResult::Unavailable,
        },
        available_runtime(),
        false,
    ));
    for id in [
        crate::diagnostics::TMUX_CLIPBOARD_ID,
        crate::diagnostics::DCS_PASSTHROUGH_ID,
        crate::diagnostics::TMUX_EXTENDED_KEYS_ID,
    ] {
        assert!(healthy.findings.iter().all(|finding| finding.id != id));
    }
}

#[test]
fn unavailable_runtime_evidence_is_honest_and_fail_open() {
    let terminal = TerminalContext {
        brand: TerminalName::WezTerm,
        env_brand: TerminalName::WezTerm,
        multiplexer: MultiplexerKind::Tmux,
        ..Default::default()
    };
    let report = view(snapshot(
        &terminal,
        TmuxProbeFacts {
            version: TmuxProbeResult::Unavailable,
            extended_keys: TmuxProbeResult::Unavailable,
            set_clipboard: TmuxProbeResult::Available("on".to_owned()),
            allow_passthrough_support: TmuxProbeResult::Available(()),
            allow_passthrough: TmuxProbeResult::Available("on".to_owned()),
            control_mode: TmuxProbeResult::Available(true),
            client_features: TmuxProbeResult::Unavailable,
        },
        DiagnosticRuntimeEvidence {
            fullscreen_active: RuntimeEvidence::Unavailable,
            kitty_flags_pushed: RuntimeEvidence::Unavailable,
            xtversion: RuntimeEvidence::Unavailable,
        },
        true,
    ));

    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.id == DiagnosticId::new("terminal", "wezterm-kitty"))
    );
    let control_mode = report
        .findings
        .iter()
        .find(|finding| finding.id == DiagnosticId::new("terminal", "control-mode"))
        .expect("control-mode finding");
    assert_eq!(
        control_mode.message,
        "Display may be limited in tmux control mode"
    );
    assert_eq!(
        report
            .probe_notes
            .iter()
            .filter(|note| note.probe.starts_with("runtime."))
            .count(),
        3
    );
}

#[test]
fn unavailable_and_error_probe_evidence_is_retained_without_findings() {
    let terminal = TerminalContext {
        brand: TerminalName::Iterm2,
        env_brand: TerminalName::Iterm2,
        multiplexer: MultiplexerKind::Tmux,
        ..Default::default()
    };
    let report = view(snapshot_with_wayland(
        &terminal,
        TmuxProbeFacts {
            version: TmuxProbeResult::Unavailable,
            extended_keys: TmuxProbeResult::Unavailable,
            set_clipboard: TmuxProbeResult::Error("server unreachable".to_owned()),
            allow_passthrough_support: TmuxProbeResult::Unsupported,
            allow_passthrough: TmuxProbeResult::Unavailable,
            control_mode: TmuxProbeResult::Unavailable,
            client_features: TmuxProbeResult::Unavailable,
        },
        available_runtime(),
        true,
        WaylandProbeFacts {
            is_wayland: true,
            data_control: TmuxProbeResult::Unavailable,
            wl_copy_available: false,
        },
    ));

    assert!(report.findings.is_empty());
    assert_eq!(
        report.facts.clipboard.delivery,
        crate::clipboard::ClipboardDelivery::Confirmed
    );
    assert_eq!(report.probe_notes.len(), 7);
    assert_eq!(report.probe_notes[0].probe, "tmux.version");
    assert_eq!(report.probe_notes[1].probe, "tmux.extended-keys");
    assert_eq!(report.probe_notes[2].status, ProbeStatus::Error);
    assert_eq!(
        report.probe_notes[2].message.as_deref(),
        Some("server unreachable")
    );
    assert_eq!(report.probe_notes[3].status, ProbeStatus::Unsupported);
    assert_eq!(report.probe_notes[4].probe, "tmux.control-mode");
    assert_eq!(report.probe_notes[5].probe, "tmux.client-features");
    assert_eq!(report.probe_notes[6].probe, "wayland.data-control");
}

fn plain_tmux() -> TmuxProbeFacts {
    TmuxProbeFacts {
        version: TmuxProbeResult::Unavailable,
        extended_keys: TmuxProbeResult::Unavailable,
        set_clipboard: TmuxProbeResult::Available("on".to_owned()),
        allow_passthrough_support: TmuxProbeResult::Available(()),
        allow_passthrough: TmuxProbeResult::Available("on".to_owned()),
        control_mode: TmuxProbeResult::Available(false),
        client_features: TmuxProbeResult::Unavailable,
    }
}

#[test]
fn local_wezterm_without_kitty_evidence_has_no_alt_enter_fallback() {
    let terminal = TerminalContext {
        brand: TerminalName::WezTerm,
        env_brand: TerminalName::WezTerm,
        ..Default::default()
    };
    let report = view(snapshot(
        &terminal,
        plain_tmux(),
        runtime(
            RuntimeEvidence::Unavailable,
            RuntimeEvidence::Available(None),
        ),
        true,
    ));

    assert!(report.facts.newline.is_none());
    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.id == DiagnosticId::new("terminal", "wezterm-kitty"))
    );
}

#[test]
fn ssh_xtversion_wezterm_without_kitty_evidence_has_no_alt_enter_fallback() {
    let terminal = TerminalContext {
        is_ssh: true,
        ..Default::default()
    };
    let report = view(snapshot(
        &terminal,
        plain_tmux(),
        runtime(
            RuntimeEvidence::Unavailable,
            RuntimeEvidence::Available(Some("WezTerm 20240203")),
        ),
        true,
    ));

    assert!(report.facts.newline.is_none());
    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.id == DiagnosticId::new("terminal", "wezterm-kitty"))
    );
}

#[test]
fn non_wezterm_without_kitty_evidence_keeps_ordinary_fallback() {
    let terminal = TerminalContext {
        brand: TerminalName::VsCode,
        env_brand: TerminalName::VsCode,
        ..Default::default()
    };
    let report = view(snapshot(
        &terminal,
        plain_tmux(),
        runtime(
            RuntimeEvidence::Unavailable,
            RuntimeEvidence::Available(None),
        ),
        true,
    ));

    assert_eq!(
        report.facts.newline,
        Some(NewlineFact::XtermJs {
            terminal: TerminalName::VsCode,
        })
    );
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.id == crate::diagnostics::NEWLINE_FALLBACK_ID)
        .expect("newline fallback finding");
    assert_eq!(finding.disposition, FindingDisposition::Recommendation);
    assert!(
        finding
            .note
            .as_deref()
            .is_some_and(|note| note.contains("Alt+Enter"))
    );
}

#[test]
fn clipboard_delivery_findings_own_remediation_while_fix_fact_stays_compatible() {
    let cases = [
        (
            crate::terminal::TerminalContext {
                is_ssh: true,
                ..Default::default()
            },
            crate::host::HostOs::Linux,
            crate::host::DisplayServer::Unknown,
            crate::clipboard::ClipboardRoute {
                native: true,
                tmux_buffer: false,
                osc52: true,
                osc52_tmux_passthrough: false,
            },
            crate::clipboard::ClipboardDelivery::Unverified,
            crate::diagnostics::CLIPBOARD_DELIVERY_UNVERIFIED_ID,
            "echoagent wrap <ssh command> or /minimal",
        ),
        (
            TerminalContext {
                brand: TerminalName::Vte,
                env_brand: TerminalName::Vte,
                ..Default::default()
            },
            crate::host::HostOs::Other,
            crate::host::DisplayServer::Unknown,
            crate::clipboard::ClipboardRoute {
                native: false,
                tmux_buffer: false,
                osc52: false,
                osc52_tmux_passthrough: false,
            },
            crate::clipboard::ClipboardDelivery::Failed,
            crate::diagnostics::CLIPBOARD_DELIVERY_UNAVAILABLE_ID,
            "/minimal",
        ),
    ];

    for (terminal, host_os, display_server, route, delivery, id, compatible_fix) in cases {
        let mut snapshot = snapshot_for_host(
            &terminal,
            plain_tmux(),
            runtime(
                RuntimeEvidence::Available(true),
                RuntimeEvidence::Available(None),
            ),
            false,
            host_os,
        );
        snapshot.display_server = display_server;
        snapshot.clipboard.route = route;
        let report = view(snapshot);
        assert_eq!(report.facts.clipboard.delivery, delivery);
        assert_eq!(
            report.facts.clipboard.fix.as_deref(),
            Some(compatible_fix),
            "JSON compatibility fact"
        );
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.id == id)
            .expect("named clipboard finding");
        assert!(
            finding
                .note
                .as_ref()
                .is_some_and(|note| !note.trim().is_empty())
        );
        assert!(!crate::diagnostics::format_doctor(&report).contains("  fix          "));
    }
}

#[test]
fn iterm2_and_vscode_clipboard_caveats_are_named_recommendations() {
    let cases = [
        (
            TerminalName::Iterm2,
            crate::diagnostics::ITERM2_CLIPBOARD_PERMISSION_ID,
            "Settings",
        ),
        (
            TerminalName::VsCode,
            crate::diagnostics::VSCODE_SSH_NON_ASCII_ID,
            "/minimal",
        ),
        (
            TerminalName::Cursor,
            crate::diagnostics::VSCODE_SSH_NON_ASCII_ID,
            "/minimal",
        ),
        (
            TerminalName::Windsurf,
            crate::diagnostics::VSCODE_SSH_NON_ASCII_ID,
            "/minimal",
        ),
        (
            TerminalName::Zed,
            crate::diagnostics::VSCODE_SSH_NON_ASCII_ID,
            "/minimal",
        ),
    ];
    for (brand, id, expected_guidance) in cases {
        let terminal = TerminalContext {
            brand,
            env_brand: brand,
            is_ssh: true,
            ..Default::default()
        };
        let report = view(snapshot_for_host(
            &terminal,
            plain_tmux(),
            runtime(
                RuntimeEvidence::Available(true),
                RuntimeEvidence::Available(None),
            ),
            false,
            crate::host::HostOs::Linux,
        ));
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.id == id)
            .expect("named clipboard caveat");
        assert_eq!(finding.disposition, FindingDisposition::Recommendation);
        assert!(
            finding
                .note
                .as_deref()
                .is_some_and(|note| note.contains(expected_guidance))
        );
    }

    let terminal = TerminalContext {
        brand: TerminalName::Ghostty,
        env_brand: TerminalName::Ghostty,
        is_ssh: true,
        ..Default::default()
    };
    let report = view(snapshot_for_host(
        &terminal,
        plain_tmux(),
        runtime(
            RuntimeEvidence::Available(true),
            RuntimeEvidence::Available(None),
        ),
        false,
        crate::host::HostOs::Linux,
    ));
    assert!(
        report
            .findings
            .iter()
            .all(|finding| finding.id != crate::diagnostics::VSCODE_SSH_NON_ASCII_ID)
    );
}

#[test]
fn available_wezterm_evidence_retains_finding_and_backslash_note() {
    let terminal = TerminalContext {
        brand: TerminalName::WezTerm,
        env_brand: TerminalName::WezTerm,
        ..Default::default()
    };
    let report = view(snapshot(&terminal, plain_tmux(), available_runtime(), true));

    assert!(report.facts.newline.is_none());
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.id == DiagnosticId::new("terminal", "wezterm-kitty"))
        .expect("WezTerm finding");
    assert!(
        finding
            .note
            .as_deref()
            .is_some_and(|note| note.contains("type `\\` and then press Enter"))
    );
}

#[test]
fn keyboard_fact_and_formatter_use_snapshot_host() {
    let terminal = TerminalContext {
        brand: TerminalName::WezTerm,
        env_brand: TerminalName::WezTerm,
        ..Default::default()
    };
    let snapshot_host = if crate::host::HostOs::current() == crate::host::HostOs::Macos {
        crate::host::HostOs::Linux
    } else {
        crate::host::HostOs::Macos
    };
    assert_ne!(snapshot_host, crate::host::HostOs::current());
    let report = view(snapshot_for_host(
        &terminal,
        plain_tmux(),
        runtime(
            RuntimeEvidence::Available(true),
            RuntimeEvidence::Available(None),
        ),
        true,
        snapshot_host,
    ));

    let keyboard = report.facts.keyboard.as_ref();
    let output = crate::diagnostics::format_doctor(&report);
    if snapshot_host == crate::host::HostOs::Macos {
        assert_eq!(keyboard.map(|fact| fact.os), Some(snapshot_host));
        assert!(output.contains("(OS rescue active)"));
    } else {
        assert!(keyboard.is_none());
        assert!(!output.contains("  keyboard     "));
    }
}

/// `RGB` in the resolved feature list is the only signal that 24-bit color
/// survives tmux. Empty output means the answer is unknown rather than
/// negative: tmux before 3.2 renders the unknown format as an empty string.
#[test]
fn client_features_decide_color_passthrough() {
    let cases = [
        (
            TmuxProbeResult::Available(
                "bpaste,ccolour,clipboard,cstyle,focus,RGB,title".to_owned(),
            ),
            TmuxColorPassthrough::Forwarded,
        ),
        (
            TmuxProbeResult::Available("RGB".to_owned()),
            TmuxColorPassthrough::Forwarded,
        ),
        (
            TmuxProbeResult::Available("bpaste,ccolour,clipboard,cstyle,focus,title".to_owned()),
            TmuxColorPassthrough::Reduced,
        ),
        (
            TmuxProbeResult::Available(String::new()),
            TmuxColorPassthrough::Unknown,
        ),
        (
            TmuxProbeResult::Available("   ".to_owned()),
            TmuxColorPassthrough::Unknown,
        ),
        (TmuxProbeResult::Unsupported, TmuxColorPassthrough::Unknown),
        (TmuxProbeResult::Unavailable, TmuxColorPassthrough::Unknown),
        (
            TmuxProbeResult::Error("tmux unreachable".to_owned()),
            TmuxColorPassthrough::Unknown,
        ),
    ];

    let actual = cases
        .iter()
        .map(|(result, _)| tmux_color_passthrough(result))
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        cases
            .iter()
            .map(|(_, expected)| *expected)
            .collect::<Vec<_>>()
    );
}
