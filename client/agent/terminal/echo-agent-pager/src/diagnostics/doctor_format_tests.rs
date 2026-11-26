//! In-TUI `/doctor` formatter tests.

use super::format_doctor;
use crate::clipboard::ClipboardRoute;
use crate::diagnostics::probes::{
    ClipboardProbeFacts, DoctorProbeSnapshot, ProbeSnapshot, TmuxProbeFacts, TmuxProbeResult,
    TuiProbeEvidence, WaylandProbeFacts,
};
use crate::diagnostics::{
    ClipboardFacts, ColorFacts, DataControlFact, DiagnosticFacts, DiagnosticReport,
    DiagnosticSnapshot, KeyboardFact, RuntimeFact, view,
};
use crate::host::HostOs;
use crate::terminal::{
    ByobuBackend, ModifierDelivery, ModifierFate, MultiplexerKind, TerminalContext, TerminalName,
};
use crate::theme::color_support::ColorLevel;

static LOCAL_ROUTE: ClipboardRoute = ClipboardRoute {
    native: true,
    tmux_buffer: false,
    osc52: false,
    osc52_tmux_passthrough: false,
};
static SSH_ROUTE: ClipboardRoute = ClipboardRoute {
    native: true,
    tmux_buffer: false,
    osc52: true,
    osc52_tmux_passthrough: false,
};
static TMUX_ROUTE: ClipboardRoute = ClipboardRoute {
    native: true,
    tmux_buffer: true,
    osc52: true,
    osc52_tmux_passthrough: true,
};

fn unavailable_tmux() -> TmuxProbeFacts {
    TmuxProbeFacts {
        version: TmuxProbeResult::Unavailable,
        extended_keys: TmuxProbeResult::Unavailable,
        set_clipboard: TmuxProbeResult::Unavailable,
        allow_passthrough_support: TmuxProbeResult::Unavailable,
        allow_passthrough: TmuxProbeResult::Unavailable,
        control_mode: TmuxProbeResult::Unavailable,
        client_features: TmuxProbeResult::Unavailable,
    }
}

fn snapshot<'a>(
    terminal: &'a TerminalContext,
    tmux: TmuxProbeFacts,
    route: &'static ClipboardRoute,
    native_tool: &'static str,
    osc52_sink_active: bool,
    color_level: ColorLevel,
    runtime: TuiProbeEvidence<'a>,
) -> DoctorProbeSnapshot<'a> {
    DoctorProbeSnapshot {
        common: ProbeSnapshot {
            terminal,
            tmux,
            wayland: WaylandProbeFacts {
                is_wayland: false,
                data_control: TmuxProbeResult::Available(false),
                wl_copy_available: false,
            },
            runtime,
        },
        clipboard: ClipboardProbeFacts {
            route: route.clone(),
            native_tool,
            osc52_sink_active,
        },
        host_os: HostOs::Macos,
        display_server: crate::host::DisplayServer::Unknown,
        container_no_display: false,
        color_level,
    }
}

fn runtime<'a>(xtversion: Option<&'a str>, kitty_flags_pushed: bool) -> TuiProbeEvidence<'a> {
    TuiProbeEvidence {
        fullscreen_active: true,
        kitty_flags_pushed,
        xtversion,
    }
}

fn ghostty(is_ssh: bool) -> TerminalContext {
    TerminalContext {
        brand: TerminalName::Ghostty,
        env_brand: TerminalName::Ghostty,
        is_ssh,
        ..Default::default()
    }
}

fn build_doctor(snapshot: DoctorProbeSnapshot<'_>) -> String {
    let report = view(DiagnosticSnapshot::from(snapshot));
    format_doctor(&report)
}

fn build_doctor_with_runtime(
    snapshot: DoctorProbeSnapshot<'_>,
    request: crate::diagnostics::TuiRuntimeRequest<'_>,
) -> String {
    let findings = crate::diagnostics::collect_tui_runtime_findings(
        &snapshot.common,
        request.notification_method,
        request.notification_protocol,
        request.notification_condition,
        request.workspace,
    );
    let mut report = view(snapshot.into());
    crate::diagnostics::merge_tui_runtime_findings(&mut report, findings);
    format_doctor(&report)
}

#[test]
fn healthy_local_output_is_stable() {
    let terminal = ghostty(false);
    let output = build_doctor(snapshot(
        &terminal,
        unavailable_tmux(),
        &LOCAL_ROUTE,
        "pbcopy",
        false,
        ColorLevel::TrueColor,
        runtime(None, true),
    ));

    assert_eq!(
        output,
        concat!(
            "Environment\n",
            "  terminal     Ghostty\n",
            "  multiplexer  None detected\n",
            "  ssh          no\n",
            "  color        truecolor\n",
            "  themes       all\n",
            "\n",
            "Clipboard\n",
            "  native       local (pbcopy)\n",
            "  tmux         off\n",
            "  osc 52       off\n",
            "  wrap         off\n",
            "  status       confirmed\n",
            "\n",
            "No issues found.\n",
        )
    );
}

#[test]
fn tmux_config_and_reload_notes_output_is_stable() {
    let terminal = TerminalContext {
        brand: TerminalName::Iterm2,
        env_brand: TerminalName::Iterm2,
        multiplexer: MultiplexerKind::Tmux,
        byobu: Some(ByobuBackend::Tmux),
        tmux_version: Some("tmux 3.4".to_owned()),
        tmux_extended_keys: Some("off".to_owned()),
        ..Default::default()
    };
    let output = build_doctor(snapshot(
        &terminal,
        TmuxProbeFacts {
            version: TmuxProbeResult::Unavailable,
            extended_keys: TmuxProbeResult::Available("off".to_owned()),
            set_clipboard: TmuxProbeResult::Available("off".to_owned()),
            allow_passthrough_support: TmuxProbeResult::Available(()),
            allow_passthrough: TmuxProbeResult::Available("off".to_owned()),
            control_mode: TmuxProbeResult::Available(false),
            client_features: TmuxProbeResult::Unavailable,
        },
        &TMUX_ROUTE,
        "pbcopy",
        false,
        ColorLevel::TrueColor,
        runtime(None, false),
    ));

    assert_eq!(
        output,
        concat!(
            "Environment\n",
            "  terminal     iTerm2\n",
            "  multiplexer  tmux\n",
            "  byobu        tmux\n",
            "  ssh          no\n",
            "  color        truecolor\n",
            "  themes       all\n",
            "\n",
            "Clipboard\n",
            "  native       local (pbcopy)\n",
            "  tmux         on\n",
            "  osc 52       supported\n",
            "  wrap         off\n",
            "  status       confirmed\n",
            "\n",
            "Issues (3)\n",
            "\n",
            "  ! terminal.tmux-clipboard  `set-clipboard` is off in tmux, so OSC 52 clipboard copies are blocked\n",
            "      Automatic setup: `echoagent doctor fix tmux-clipboard`\n",
            "      Add `set -g set-clipboard on` to ~/.byobu/.tmux.conf\n",
            "      Note: Reload tmux with `tmux source-file ~/.byobu/.tmux.conf`, or restart the tmux server.\n",
            "\n",
            "  ! terminal.dcs-passthrough  `allow-passthrough` is off in tmux, which can block clipboard copies in nested sessions\n",
            "      Automatic setup: `echoagent doctor fix dcs-passthrough`\n",
            "      Add `set -wg allow-passthrough on` to ~/.byobu/.tmux.conf\n",
            "      Note: Reload tmux with `tmux source-file ~/.byobu/.tmux.conf`, or restart the tmux server.\n",
            "\n",
            "  ! terminal.tmux-extended-keys  `extended-keys` is off in tmux, so some shortcuts may not work\n",
            "      Automatic setup: `echoagent doctor fix tmux-extended-keys`\n",
            "      Add `set -g extended-keys on` to ~/.byobu/.tmux.conf\n",
            "      Note: Reload tmux with `tmux source-file ~/.byobu/.tmux.conf`, or restart the tmux server.\n",
        )
    );
}

#[test]
fn limited_color_output_is_stable() {
    let terminal = ghostty(false);
    let output = build_doctor(snapshot(
        &terminal,
        unavailable_tmux(),
        &LOCAL_ROUTE,
        "pbcopy",
        false,
        ColorLevel::Ansi256,
        runtime(None, true),
    ));

    assert_eq!(
        output,
        concat!(
            "Environment\n",
            "  terminal     Ghostty\n",
            "  multiplexer  None detected\n",
            "  ssh          no\n",
            "  color        256\n",
            "  themes       2/5: echo-agent-night, echo-agent-day\n",
            "\n",
            "Clipboard\n",
            "  native       local (pbcopy)\n",
            "  tmux         off\n",
            "  osc 52       off\n",
            "  wrap         off\n",
            "  status       confirmed\n",
            "\n",
            "Issues (1)\n",
            "\n",
            "  ! terminal.limited-color  This terminal reports 256 color, so truecolor themes are unavailable\n",
            "      Run: `export COLORTERM=truecolor`\n",
            "      Note: Add this export to your shell startup file, such as `~/.zshrc` or `~/.bashrc`, then restart EchoAgent.\n",
        )
    );
}

#[test]
fn unwrapped_ssh_recommendation_with_no_issues_output_is_stable() {
    let terminal = ghostty(true);
    let output = build_doctor(snapshot(
        &terminal,
        unavailable_tmux(),
        &SSH_ROUTE,
        "pbcopy",
        false,
        ColorLevel::TrueColor,
        runtime(None, true),
    ));

    assert_eq!(
        output,
        concat!(
            "Environment\n",
            "  terminal     Ghostty\n",
            "  multiplexer  None detected\n",
            "  ssh          yes\n",
            "  color        truecolor\n",
            "  themes       all\n",
            "\n",
            "Clipboard\n",
            "  native       remote (pbcopy)\n",
            "  tmux         off\n",
            "  osc 52       supported\n",
            "  wrap         off\n",
            "  status       confirmed\n",
            "\n",
            "No issues found.\n",
            "\n",
            "Recommendations\n",
            "\n",
            "  i terminal.ssh-wrap  Use local SSH wrapping for more reliable clipboard copy and terminal recovery\n",
            "      Automatic setup: `echoagent doctor fix ssh-wrap`\n",
            "      One-off: `echoagent wrap ssh <host>`\n",
            "      Note: Run this on your local computer instead of plain `ssh`. It forwards copies to your local clipboard and restores terminal modes if the connection drops.\n",
        )
    );
}

#[test]
fn wrapped_ssh_output_has_no_recommendation() {
    let terminal = ghostty(true);
    let output = build_doctor(snapshot(
        &terminal,
        unavailable_tmux(),
        &SSH_ROUTE,
        "pbcopy",
        true,
        ColorLevel::TrueColor,
        runtime(None, true),
    ));

    assert_eq!(
        output,
        concat!(
            "Environment\n",
            "  terminal     Ghostty\n",
            "  multiplexer  None detected\n",
            "  ssh          yes\n",
            "  color        truecolor\n",
            "  themes       all\n",
            "\n",
            "Clipboard\n",
            "  native       remote (pbcopy)\n",
            "  tmux         off\n",
            "  osc 52       supported\n",
            "  wrap         on\n",
            "  status       confirmed\n",
            "\n",
            "No issues found.\n",
        )
    );
}

#[test]
fn wezterm_xtversion_runtime_evidence_output_is_stable() {
    let terminal = TerminalContext {
        is_ssh: true,
        ..Default::default()
    };
    let output = build_doctor(snapshot(
        &terminal,
        unavailable_tmux(),
        &SSH_ROUTE,
        "pbcopy",
        true,
        ColorLevel::TrueColor,
        runtime(Some("WezTerm 20240203-110809"), false),
    ));

    assert_eq!(
        output,
        concat!(
            "Environment\n",
            "  terminal     Unknown\n",
            "  xtversion    WezTerm 20240203-110809\n",
            "  multiplexer  None detected\n",
            "  ssh          yes\n",
            "  color        truecolor\n",
            "  themes       all\n",
            "\n",
            "Clipboard\n",
            "  native       remote (pbcopy)\n",
            "  tmux         off\n",
            "  osc 52       supported\n",
            "  wrap         on\n",
            "  status       confirmed\n",
            "\n",
            "Issues (1)\n",
            "\n",
            "  ! terminal.wezterm-kitty  Shift+Enter can't insert a newline in WezTerm over SSH\n",
            "      Note: For this session, type `\\` and then press Enter. EchoAgent can't negotiate the Kitty keyboard protocol over SSH yet. `enable_kitty_keyboard = true` applies only to local WezTerm sessions.\n",
        )
    );
}

#[test]
fn unavailable_and_error_probes_do_not_create_false_issues() {
    let terminal = TerminalContext {
        brand: TerminalName::Iterm2,
        env_brand: TerminalName::Iterm2,
        multiplexer: MultiplexerKind::Tmux,
        tmux_version: Some("tmux 3.4".to_owned()),
        ..Default::default()
    };
    let output = build_doctor(snapshot(
        &terminal,
        TmuxProbeFacts {
            version: TmuxProbeResult::Unavailable,
            extended_keys: TmuxProbeResult::Unavailable,
            set_clipboard: TmuxProbeResult::Error("tmux server unreachable".to_owned()),
            allow_passthrough_support: TmuxProbeResult::Unavailable,
            allow_passthrough: TmuxProbeResult::Error("query failed".to_owned()),
            control_mode: TmuxProbeResult::Unavailable,
            client_features: TmuxProbeResult::Unavailable,
        },
        &TMUX_ROUTE,
        "pbcopy",
        false,
        ColorLevel::TrueColor,
        runtime(None, false),
    ));

    assert_eq!(
        output,
        concat!(
            "Environment\n",
            "  terminal     iTerm2\n",
            "  multiplexer  tmux\n",
            "  ssh          no\n",
            "  color        truecolor\n",
            "  themes       all\n",
            "\n",
            "Clipboard\n",
            "  native       local (pbcopy)\n",
            "  tmux         on\n",
            "  osc 52       supported\n",
            "  wrap         off\n",
            "  status       confirmed\n",
            "\n",
            "No issues found.\n",
        )
    );
}

#[test]
fn vscode_newline_output_is_platform_neutral() {
    let terminal = TerminalContext {
        brand: TerminalName::VsCode,
        env_brand: TerminalName::VsCode,
        ..Default::default()
    };
    let output = build_doctor(snapshot(
        &terminal,
        unavailable_tmux(),
        &LOCAL_ROUTE,
        "pbcopy",
        false,
        ColorLevel::TrueColor,
        runtime(None, false),
    ));

    assert_eq!(
        output,
        concat!(
            "Environment\n",
            "  terminal     VS Code\n",
            "  multiplexer  None detected\n",
            "  ssh          no\n",
            "  color        truecolor\n",
            "  themes       all\n",
            "  newline      Alt+Enter (VS Code: xterm.js can't distinguish Shift+Enter)\n",
            "\n",
            "Clipboard\n",
            "  native       local (pbcopy)\n",
            "  tmux         off\n",
            "  osc 52       off\n",
            "  wrap         off\n",
            "  status       confirmed\n",
            "\n",
            "No issues found.\n",
            "\n",
            "Recommendations\n",
            "\n",
            "  i terminal.newline-fallback  Shift+Enter can't insert a newline in this xterm.js terminal\n",
            "      Note: Use Alt+Enter to insert a newline in VS Code. xterm.js sends Shift+Enter as Enter in this setup.\n",
        )
    );
}

#[test]
fn runtime_merge_does_not_duplicate_view_findings() {
    let terminal = TerminalContext {
        brand: TerminalName::Iterm2,
        env_brand: TerminalName::Iterm2,
        multiplexer: MultiplexerKind::Tmux,
        tmux_extended_keys: Some("off".to_owned()),
        ..Default::default()
    };
    let workspace = tempfile::tempdir().unwrap();
    let output = build_doctor_with_runtime(
        snapshot(
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
            &TMUX_ROUTE,
            "pbcopy",
            false,
            ColorLevel::Ansi256,
            runtime(None, false),
        ),
        crate::diagnostics::TuiRuntimeRequest {
            workspace: workspace.path(),
            notification_method: crate::notifications::NotificationMethod::Auto,
            notification_protocol: crate::notifications::protocol::NotificationProtocol::Bel,
            notification_condition: crate::notifications::NotificationCondition::Always,
        },
    );

    for id in [
        "terminal.tmux-clipboard",
        "terminal.dcs-passthrough",
        "terminal.tmux-extended-keys",
        "terminal.limited-color",
    ] {
        assert_eq!(output.matches(id).count(), 1, "{id}:\n{output}");
    }
    assert!(output.contains("Issues (4)"), "{output}");
}

#[test]
fn runtime_startup_findings_are_visible_with_useful_doctor_content() {
    let terminal = TerminalContext::default();
    let workspace = tempfile::tempdir().unwrap();
    let output = build_doctor_with_runtime(
        snapshot(
            &terminal,
            unavailable_tmux(),
            &LOCAL_ROUTE,
            "pbcopy",
            false,
            ColorLevel::TrueColor,
            runtime(None, true),
        ),
        crate::diagnostics::TuiRuntimeRequest {
            workspace: workspace.path(),
            notification_method: crate::notifications::NotificationMethod::Auto,
            notification_protocol: crate::notifications::protocol::NotificationProtocol::Bel,
            notification_condition: crate::notifications::NotificationCondition::Unfocused,
        },
    );

    assert!(output.contains("EchoAgent is using the terminal bell"));
    assert!(output.contains("If the bell works for you"));
    assert!(output.contains("may not report focus changes"));
    assert!(output.contains(&crate::util::display_user_echo_agent_path("config.toml")));
    assert_eq!(output.matches("notifications.protocol-fallback").count(), 1);
    assert_eq!(
        output
            .matches("notifications.focus-tracking-unavailable")
            .count(),
        1
    );
    assert!(!output.contains("No issues found."));
    assert!(output.contains("Issues (2)"));
    assert!(output.contains("terminal.newline-fallback"));
    assert!(output.contains("Recommendations"));
}

#[test]
fn runtime_findings_merge_before_single_formatter_orders_issues_before_recommendations() {
    let terminal = TerminalContext {
        brand: TerminalName::Unknown,
        env_brand: TerminalName::Unknown,
        is_ssh: true,
        ..Default::default()
    };
    let workspace = tempfile::tempdir().unwrap();
    let output = build_doctor_with_runtime(
        snapshot(
            &terminal,
            unavailable_tmux(),
            &SSH_ROUTE,
            "pbcopy",
            false,
            ColorLevel::TrueColor,
            runtime(None, true),
        ),
        crate::diagnostics::TuiRuntimeRequest {
            workspace: workspace.path(),
            notification_method: crate::notifications::NotificationMethod::Auto,
            notification_protocol: crate::notifications::protocol::NotificationProtocol::Bel,
            notification_condition: crate::notifications::NotificationCondition::Unfocused,
        },
    );

    let issue = output.find("EchoAgent is using the terminal bell").unwrap();
    let recommendation = output.find("Recommendations").unwrap();
    assert!(issue < recommendation);
    assert!(!output.contains("No issues found."));
    assert_eq!(output.matches("Issues (").count(), 1);
}

#[test]
fn legacy_fact_only_clipboard_issue_never_claims_no_issues() {
    let terminal = ghostty(false);
    let mut report = view(DiagnosticSnapshot::from(snapshot(
        &terminal,
        unavailable_tmux(),
        &LOCAL_ROUTE,
        "pbcopy",
        false,
        ColorLevel::TrueColor,
        runtime(None, true),
    )));
    report.facts.clipboard.delivery = crate::clipboard::ClipboardDelivery::Failed;
    assert_eq!(report.issue_count(), 1);
    let output = format_doctor(&report);
    assert!(output.contains("An issue is shown in the Clipboard status above."));
    assert!(!output.contains("No issues found."));
}

#[test]
fn keyboard_fact_formats_from_explicit_target_evidence() {
    let report = DiagnosticReport {
        facts: DiagnosticFacts {
            terminal: TerminalName::WezTerm,
            xtversion: RuntimeFact::NoReply,
            multiplexer: MultiplexerKind::Undetected,
            byobu: None,
            ssh: false,
            tmux: crate::diagnostics::TmuxFacts {
                extended_keys: crate::diagnostics::TmuxOptionFact::Unavailable,
                set_clipboard: crate::diagnostics::TmuxOptionFact::Unavailable,
                allow_passthrough_support: crate::diagnostics::TmuxSupportFact::Unavailable,
                allow_passthrough: crate::diagnostics::TmuxOptionFact::Unavailable,
                color_passthrough: crate::diagnostics::TmuxColorPassthrough::Unknown,
            },
            color: ColorFacts {
                level: RuntimeFact::Available(ColorLevel::TrueColor),
                available_themes: crate::theme::ThemeKind::ALL.to_vec(),
                total_themes: crate::theme::ThemeKind::ALL.len(),
            },
            keyboard: Some(KeyboardFact {
                modifier_delivery: ModifierDelivery::new_for_test(
                    ModifierFate::Dropped,
                    ModifierFate::Native,
                ),
                os: HostOs::Macos,
            }),
            newline: None,
            clipboard: ClipboardFacts {
                native_route: true,
                native_tool: "pbcopy".to_owned(),
                native_preflight: crate::clipboard::NativeClipboardPreflight::LocalAvailable,
                tmux_route: false,
                osc52_route: false,
                osc52_capability: crate::clipboard::Osc52Capability::Supported,
                wrap_sink: false,
                display_server: crate::host::DisplayServer::Unknown,
                container_no_display: false,
                data_control: DataControlFact::NotApplicable,
                delivery: crate::clipboard::ClipboardDelivery::Confirmed,
                fix: None,
            },
            voice: None,
        },
        findings: Vec::new(),
        probe_notes: Vec::new(),
    };

    assert_eq!(
        format_doctor(&report),
        concat!(
            "Environment\n",
            "  terminal     WezTerm\n",
            "  multiplexer  None detected\n",
            "  ssh          no\n",
            "  color        truecolor\n",
            "  themes       all\n",
            "  keyboard     cmd=dropped, opt=native (OS rescue active)\n",
            "\n",
            "Clipboard\n",
            "  native       local (pbcopy)\n",
            "  tmux         off\n",
            "  osc 52       off\n",
            "  wrap         off\n",
            "  status       confirmed\n",
            "\n",
            "No issues found.\n",
        )
    );
}
