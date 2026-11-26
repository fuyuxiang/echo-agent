//! Interpretation of terminal probe snapshots.

use crate::diagnostics::probes::{
    CommonProbeSnapshot, DiagnosticRuntimeEvidence, DoctorProbeSnapshot, ProbeSnapshot,
    RuntimeEvidence, StandaloneDiagnosticSnapshot, TmuxProbeResult,
};
use crate::diagnostics::{
    ClipboardFacts, ColorFacts, DataControlFact, DiagnosticFacts, DiagnosticFinding, DiagnosticId,
    DiagnosticReport, FindingDisposition, KeyboardFact, ManualRemediation, NewlineFact, ProbeNote,
    ProbeStatus, RuntimeFact, TerminalWarning, TmuxColorPassthrough, TmuxFacts, TmuxOptionFact,
    TmuxSupportFact, WarningCategory,
};
use crate::terminal::TerminalName;

pub struct DiagnosticSnapshot<'a> {
    pub common: CommonProbeSnapshot<'a>,
    pub clipboard: crate::diagnostics::probes::ClipboardProbeFacts,
    pub host_os: crate::host::HostOs,
    pub display_server: crate::host::DisplayServer,
    pub container_no_display: bool,
    pub color_level: RuntimeEvidence<crate::theme::color_support::ColorLevel>,
    pub runtime: DiagnosticRuntimeEvidence<'a>,
}

impl<'a> DiagnosticSnapshot<'a> {
    pub fn from_parts(
        common: ProbeSnapshot<'a>,
        clipboard: crate::diagnostics::probes::ClipboardProbeFacts,
        host_os: crate::host::HostOs,
        display_server: crate::host::DisplayServer,
        container_no_display: bool,
        color_level: crate::theme::color_support::ColorLevel,
        runtime: DiagnosticRuntimeEvidence<'a>,
    ) -> Self {
        Self {
            common: CommonProbeSnapshot {
                terminal: common.terminal,
                tmux: common.tmux,
                wayland: common.wayland,
            },
            clipboard,
            host_os,
            display_server,
            container_no_display,
            color_level: RuntimeEvidence::Available(color_level),
            runtime,
        }
    }
}

impl<'a> From<DoctorProbeSnapshot<'a>> for DiagnosticSnapshot<'a> {
    fn from(snapshot: DoctorProbeSnapshot<'a>) -> Self {
        let runtime = snapshot.common.runtime.into();
        Self {
            common: CommonProbeSnapshot {
                terminal: snapshot.common.terminal,
                tmux: snapshot.common.tmux,
                wayland: snapshot.common.wayland,
            },
            clipboard: snapshot.clipboard,
            host_os: snapshot.host_os,
            display_server: snapshot.display_server,
            container_no_display: snapshot.container_no_display,
            color_level: RuntimeEvidence::Available(snapshot.color_level),
            runtime,
        }
    }
}

impl<'a> From<StandaloneDiagnosticSnapshot<'a>> for DiagnosticSnapshot<'a> {
    fn from(snapshot: StandaloneDiagnosticSnapshot<'a>) -> Self {
        Self {
            common: snapshot.common,
            clipboard: snapshot.clipboard,
            host_os: snapshot.host_os,
            display_server: snapshot.display_server,
            container_no_display: snapshot.container_no_display,
            color_level: snapshot.color_level,
            runtime: DiagnosticRuntimeEvidence {
                fullscreen_active: RuntimeEvidence::Unavailable,
                kitty_flags_pushed: RuntimeEvidence::Unavailable,
                xtversion: RuntimeEvidence::Unavailable,
            },
        }
    }
}

pub fn view(snapshot: DiagnosticSnapshot<'_>) -> DiagnosticReport {
    let ctx = snapshot.common.terminal;
    let wezterm_warning = wezterm_warning(&snapshot);
    let suppress_newline = wezterm_warning.is_some()
        || matches!(
            snapshot.runtime.kitty_flags_pushed,
            RuntimeEvidence::Unavailable
        ) && super::wezterm_shape(
            snapshot.common.terminal,
            runtime_xtversion(snapshot.runtime.xtversion),
        )
        .is_some();

    let mut warnings = startup_warnings(&snapshot);
    warnings.extend(super::diagnose_wayland_data_control_from_common(
        &snapshot.common,
    ));
    warnings.extend(wezterm_warning);
    warnings.extend(super::color_support_warning(
        snapshot.color_level,
        ctx.brand,
        tmux_color_passthrough(&snapshot.common.tmux.client_features),
        ctx.is_tmux_backed(),
        &ctx.tmux_config_path(),
    ));

    let (facts, clipboard_recovery) = facts(&snapshot, suppress_newline);
    let mut findings = warnings
        .into_iter()
        .filter_map(finding_from_warning)
        .collect::<Vec<_>>();
    findings.extend(clipboard_findings(&facts, ctx, clipboard_recovery));
    findings.extend(newline_finding(&facts));
    findings.extend(
        super::ssh_wrap_hint(
            ctx.is_ssh,
            snapshot.clipboard.osc52_sink_active,
            ctx.is_official_vscode_remote,
        )
        .and_then(recommendation),
    );

    DiagnosticReport {
        facts,
        findings,
        probe_notes: probe_notes(&snapshot),
    }
}

fn startup_warnings(snapshot: &DiagnosticSnapshot<'_>) -> Vec<TerminalWarning> {
    let fullscreen_active = match snapshot.runtime.fullscreen_active {
        RuntimeEvidence::Available(fullscreen_active) => Some(fullscreen_active),
        RuntimeEvidence::Unavailable => None,
    };
    super::collect_startup_warnings_from(
        snapshot.common.terminal,
        &snapshot.common.tmux,
        fullscreen_active,
    )
}

fn wezterm_warning(snapshot: &DiagnosticSnapshot<'_>) -> Option<TerminalWarning> {
    let RuntimeEvidence::Available(kitty_flags_pushed) = snapshot.runtime.kitty_flags_pushed else {
        return None;
    };
    super::wezterm_kitty_keyboard_warning_from(
        snapshot.common.terminal,
        kitty_flags_pushed,
        runtime_xtversion(snapshot.runtime.xtversion),
    )
}

fn runtime_xtversion(evidence: RuntimeEvidence<Option<&str>>) -> Option<&str> {
    match evidence {
        RuntimeEvidence::Available(xtversion) => xtversion,
        RuntimeEvidence::Unavailable => None,
    }
}

fn facts(
    snapshot: &DiagnosticSnapshot<'_>,
    suppress_newline: bool,
) -> (DiagnosticFacts, ClipboardRecovery) {
    let ctx = snapshot.common.terminal;
    let available_themes = match snapshot.color_level {
        RuntimeEvidence::Available(color_level) => crate::theme::ThemeKind::ALL
            .iter()
            .copied()
            .filter(|kind| color_level.has_truecolor() || !kind.requires_truecolor())
            .collect(),
        RuntimeEvidence::Unavailable => Vec::new(),
    };
    let keyboard_capabilities =
        crate::terminal::keyboard_capabilities_for_host(ctx.brand, snapshot.host_os);
    let keyboard = (keyboard_capabilities
        .modifier_delivery
        .benefits_from_rescue()
        || keyboard_capabilities.enter_needs_rescue())
    .then_some(KeyboardFact {
        modifier_delivery: keyboard_capabilities.modifier_delivery,
        os: snapshot.host_os,
    });
    let newline = (ctx.shift_enter_unavailable() && !suppress_newline).then(|| {
        if ctx.vte_version.is_some() || ctx.brand == TerminalName::Vte {
            NewlineFact::Vte {
                version: ctx.vte_version.clone(),
            }
        } else if ctx.brand.is_vscode_family() {
            NewlineFact::XtermJs {
                terminal: ctx.brand,
            }
        } else {
            NewlineFact::NoKittyKeyboardProtocol
        }
    });
    let data_control = if !snapshot.common.wayland.is_wayland {
        DataControlFact::NotApplicable
    } else {
        match &snapshot.common.wayland.data_control {
            TmuxProbeResult::Available(true) => DataControlFact::Available,
            TmuxProbeResult::Available(false) => DataControlFact::Missing,
            TmuxProbeResult::Unavailable | TmuxProbeResult::Unsupported => {
                DataControlFact::Unavailable
            }
            TmuxProbeResult::Error(_) => DataControlFact::Error,
        }
    };
    let (clipboard, clipboard_recovery) = clipboard_facts(snapshot, data_control);

    (
        DiagnosticFacts {
            terminal: ctx.brand,
            xtversion: match snapshot.runtime.xtversion {
                RuntimeEvidence::Available(Some(xtversion)) => {
                    RuntimeFact::Available(xtversion.to_owned())
                }
                RuntimeEvidence::Available(None) => RuntimeFact::NoReply,
                RuntimeEvidence::Unavailable => RuntimeFact::Unavailable,
            },
            multiplexer: ctx.multiplexer,
            byobu: ctx.byobu,
            ssh: ctx.is_ssh,
            tmux: TmuxFacts {
                extended_keys: tmux_option_fact(&snapshot.common.tmux.extended_keys),
                set_clipboard: tmux_option_fact(&snapshot.common.tmux.set_clipboard),
                allow_passthrough_support: tmux_support_fact(
                    &snapshot.common.tmux.allow_passthrough_support,
                ),
                allow_passthrough: tmux_option_fact(&snapshot.common.tmux.allow_passthrough),
                color_passthrough: tmux_color_passthrough(&snapshot.common.tmux.client_features),
            },
            color: ColorFacts {
                level: match snapshot.color_level {
                    RuntimeEvidence::Available(level) => RuntimeFact::Available(level),
                    RuntimeEvidence::Unavailable => RuntimeFact::Unavailable,
                },
                available_themes,
                total_themes: crate::theme::ThemeKind::ALL.len(),
            },
            keyboard,
            newline,
            clipboard,
            voice: None,
        },
        clipboard_recovery,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClipboardRecovery {
    Confirmed,
    UnverifiedSsh,
    UnverifiedContainer,
    UnverifiedOther,
    UnavailableSsh,
    UnavailableContainer,
    UnavailableLocal,
}

impl ClipboardRecovery {
    fn classify(delivery: crate::clipboard::ClipboardDelivery, ssh: bool, container: bool) -> Self {
        use crate::clipboard::ClipboardDelivery;
        match (delivery, ssh, container) {
            (ClipboardDelivery::Confirmed, _, _) => Self::Confirmed,
            (ClipboardDelivery::Unverified, true, _) => Self::UnverifiedSsh,
            (ClipboardDelivery::Unverified, false, true) => Self::UnverifiedContainer,
            (ClipboardDelivery::Unverified, false, false) => Self::UnverifiedOther,
            (ClipboardDelivery::Failed, true, _) => Self::UnavailableSsh,
            (ClipboardDelivery::Failed, false, true) => Self::UnavailableContainer,
            (ClipboardDelivery::Failed, false, false) => Self::UnavailableLocal,
        }
    }

    fn legacy_fix(self) -> Option<&'static str> {
        match self {
            Self::Confirmed => None,
            Self::UnverifiedSsh | Self::UnavailableSsh => {
                Some("echoagent wrap <ssh command> or /minimal")
            }
            Self::UnverifiedContainer | Self::UnavailableContainer => {
                Some("echoagent wrap <command> or /minimal")
            }
            Self::UnverifiedOther => Some("echoagent wrap or /minimal"),
            Self::UnavailableLocal => Some("/minimal"),
        }
    }
}

fn clipboard_facts(
    snapshot: &DiagnosticSnapshot<'_>,
    data_control: DataControlFact,
) -> (ClipboardFacts, ClipboardRecovery) {
    use crate::clipboard::{ClipboardEnvironment, expected_delivery};

    let route = &snapshot.clipboard.route;
    let environment = ClipboardEnvironment {
        brand: snapshot.common.terminal.brand,
        host_os: snapshot.host_os,
        display_server: snapshot.display_server,
        remote: snapshot.common.terminal.is_ssh,
        container: snapshot.container_no_display,
        osc52_sink: snapshot.clipboard.osc52_sink_active,
        wayland_data_control: matches!(
            snapshot.common.wayland.data_control,
            TmuxProbeResult::Available(true)
        ),
        wl_copy_available: snapshot.common.wayland.wl_copy_available,
    };
    let native_preflight = crate::clipboard::native_clipboard_preflight(route.native, environment);
    let delivery = expected_delivery(
        native_preflight,
        route.tmux_buffer,
        route.osc52,
        environment,
    );
    let recovery = ClipboardRecovery::classify(
        delivery,
        snapshot.common.terminal.is_ssh,
        snapshot.container_no_display,
    );

    (
        ClipboardFacts {
            native_route: route.native,
            native_tool: snapshot.clipboard.native_tool.to_owned(),
            native_preflight,
            tmux_route: route.tmux_buffer,
            osc52_route: route.osc52,
            osc52_capability: environment.osc52_capability(),
            wrap_sink: snapshot.clipboard.osc52_sink_active,
            display_server: snapshot.display_server,
            container_no_display: snapshot.container_no_display,
            data_control,
            delivery,
            fix: recovery.legacy_fix().map(str::to_owned),
        },
        recovery,
    )
}

pub(crate) fn finding_from_warning(warning: TerminalWarning) -> Option<DiagnosticFinding> {
    let disposition = disposition_for(warning.category);
    finding(warning, disposition)
}

pub(crate) const fn disposition_for(category: WarningCategory) -> FindingDisposition {
    match category {
        WarningCategory::SshWithoutWrap => FindingDisposition::Recommendation,
        _ => FindingDisposition::Issue,
    }
}

fn recommendation(warning: TerminalWarning) -> Option<DiagnosticFinding> {
    finding(warning, FindingDisposition::Recommendation)
}

fn manual_finding(
    id: DiagnosticId,
    disposition: FindingDisposition,
    message: impl Into<String>,
    note: impl Into<String>,
) -> DiagnosticFinding {
    DiagnosticFinding {
        id,
        disposition,
        message: message.into(),
        remediation: None,
        automatic_remediation: None,
        note: Some(note.into()),
    }
}

fn clipboard_findings(
    facts: &DiagnosticFacts,
    ctx: &crate::terminal::TerminalContext,
    recovery: ClipboardRecovery,
) -> Vec<DiagnosticFinding> {
    let mut findings = Vec::new();
    match recovery {
        ClipboardRecovery::Confirmed => {}
        ClipboardRecovery::UnverifiedSsh => findings.push(manual_finding(
            crate::diagnostics::CLIPBOARD_DELIVERY_UNVERIFIED_ID,
            FindingDisposition::Issue,
            "EchoAgent can't verify this clipboard route across the remote boundary",
            "When you copy, EchoAgent sends OSC 52 but can't confirm that the outer terminal accepted \
             it. Each copy is also saved to a backup file; the copy message shows the path. If \
             paste fails, run `echoagent wrap ssh <host>` on your local computer or use `/minimal`. \
             For repeated SSH sessions, run `echoagent doctor fix ssh-wrap` on your local computer.",
        )),
        ClipboardRecovery::UnverifiedContainer => findings.push(manual_finding(
            crate::diagnostics::CLIPBOARD_DELIVERY_UNVERIFIED_ID,
            FindingDisposition::Issue,
            "EchoAgent can't verify this clipboard route across the container boundary",
            "When you copy, EchoAgent sends OSC 52 but can't confirm that the outer terminal accepted \
             it. Each copy is also saved to a backup file; the copy message shows the path. If \
             paste fails, start the container command with local `echoagent wrap <command>`, or use \
             `/minimal`.",
        )),
        ClipboardRecovery::UnverifiedOther => findings.push(manual_finding(
            crate::diagnostics::CLIPBOARD_DELIVERY_UNVERIFIED_ID,
            FindingDisposition::Issue,
            "EchoAgent can't verify this clipboard route",
            "Each copy is also saved to a backup file; the copy message shows the path. For a \
             remote or container command, use local `echoagent wrap <command>`. You can also use \
             `/minimal` to select text in the terminal.",
        )),
        ClipboardRecovery::UnavailableSsh => findings.push(manual_finding(
            crate::diagnostics::CLIPBOARD_DELIVERY_UNAVAILABLE_ID,
            FindingDisposition::Issue,
            "This clipboard route can't reach the target clipboard",
            "When you copy, EchoAgent saves the text to the backup file shown in the copy message. To \
             copy directly, run `echoagent wrap ssh <host>` on your local computer. For repeated SSH \
             sessions, run `echoagent doctor fix ssh-wrap` there. You can also use `/copy <file>` or \
             `/minimal`.",
        )),
        ClipboardRecovery::UnavailableContainer => findings.push(manual_finding(
            crate::diagnostics::CLIPBOARD_DELIVERY_UNAVAILABLE_ID,
            FindingDisposition::Issue,
            "This clipboard route can't reach the target clipboard",
            "When you copy, EchoAgent saves the text to the backup file shown in the copy message. \
             Start the container command with local `echoagent wrap <command>`, use `/copy <file>`, or \
             use `/minimal`.",
        )),
        ClipboardRecovery::UnavailableLocal => findings.push(manual_finding(
            crate::diagnostics::CLIPBOARD_DELIVERY_UNAVAILABLE_ID,
            FindingDisposition::Issue,
            "This clipboard route can't reach the target clipboard",
            "When you copy, EchoAgent saves the text to the backup file shown in the copy message. Use \
             `/copy <file>` or `/minimal`, then check the native clipboard tool listed above.",
        )),
    }

    if ctx.brand.is_vscode_family()
        && ctx.is_ssh
        && facts.clipboard.osc52_route
        && !facts.clipboard.wrap_sink
    {
        findings.push(manual_finding(
            crate::diagnostics::VSCODE_SSH_NON_ASCII_ID,
            FindingDisposition::Recommendation,
            "This remote editor may change non-ASCII text copied with OSC 52",
            "If pasted non-ASCII text is incorrect, use `/minimal` and select text in the \
             terminal. ASCII copy and the backup file shown after the copy remain available.",
        ));
    }

    if ctx.brand == TerminalName::Iterm2
        && (ctx.is_ssh
            || facts.clipboard.native_preflight
                != crate::clipboard::NativeClipboardPreflight::LocalAvailable)
        && facts.clipboard.osc52_route
        && !facts.clipboard.wrap_sink
    {
        findings.push(manual_finding(
            crate::diagnostics::ITERM2_CLIPBOARD_PERMISSION_ID,
            FindingDisposition::Recommendation,
            "iTerm2 may block OSC 52 clipboard access",
            "In iTerm2, open Settings → General → Selection and turn on “Applications in \
             terminal may access clipboard.” EchoAgent can't read this setting, so check it there if \
             copies don't paste.",
        ));
    }
    findings
}

fn newline_finding(facts: &DiagnosticFacts) -> Option<DiagnosticFinding> {
    let newline = facts.newline.as_ref()?;
    let (message, note) = match newline {
        NewlineFact::Vte { version } => (
            "Shift+Enter can't insert a newline in this VTE terminal",
            match version {
                Some(version) => format!(
                    "Use Alt+Enter to insert a newline. This terminal reports VTE {version}. \
                     Upgrade to VTE 0.82 or later to use Shift+Enter."
                ),
                None => "Use Alt+Enter to insert a newline. Upgrade to VTE 0.82 or later to use \
                         Shift+Enter."
                    .to_owned(),
            },
        ),
        NewlineFact::XtermJs { terminal } => (
            "Shift+Enter can't insert a newline in this xterm.js terminal",
            format!(
                "Use Alt+Enter to insert a newline in {terminal}. xterm.js sends Shift+Enter as \
                 Enter in this setup."
            ),
        ),
        NewlineFact::NoKittyKeyboardProtocol => (
            "Shift+Enter can't insert a newline because the keyboard protocol is unavailable",
            "Use Alt+Enter to insert a newline. If your terminal supports the Kitty keyboard \
             protocol, enable it and restart EchoAgent."
                .to_owned(),
        ),
    };
    Some(manual_finding(
        crate::diagnostics::NEWLINE_FALLBACK_ID,
        FindingDisposition::Recommendation,
        message,
        note,
    ))
}

fn finding(warning: TerminalWarning, disposition: FindingDisposition) -> Option<DiagnosticFinding> {
    let id = id_for(warning.category)?;
    Some(DiagnosticFinding {
        id,
        disposition,
        message: warning.message,
        remediation: warning.fix.map(|fix| ManualRemediation {
            fix,
            config_path: warning.config_path,
        }),
        automatic_remediation: crate::diagnostics::automatic_remediation_for(id),
        note: warning.note,
    })
}

pub(crate) const fn id_for(category: WarningCategory) -> Option<DiagnosticId> {
    let item = match category {
        WarningCategory::Clipboard => "tmux-clipboard",
        WarningCategory::DcsPassthrough => "dcs-passthrough",
        WarningCategory::ControlMode => "control-mode",
        WarningCategory::ByobuScreen => "byobu-screen",
        WarningCategory::UnsupportedTerminal => "unsupported-emulator",
        WarningCategory::TmuxExtendedKeysOff => "tmux-extended-keys",
        WarningCategory::WaylandNoDataControl => "wayland-data-control",
        WarningCategory::WezTermKittyKeyboardOff => "wezterm-kitty",
        WarningCategory::LimitedColorSupport => "limited-color",
        WarningCategory::TmuxColorReduced => "tmux-truecolor",
        WarningCategory::SshWithoutWrap => "ssh-wrap",
        WarningCategory::NotificationProtocolFallback => {
            return Some(crate::diagnostics::NOTIFICATION_PROTOCOL_FALLBACK_ID);
        }
        WarningCategory::FocusTrackingUnavailable => {
            return Some(crate::diagnostics::FOCUS_TRACKING_UNAVAILABLE_ID);
        }
        WarningCategory::SandboxProfileConflict => {
            return Some(crate::diagnostics::SANDBOX_PROFILE_CONFLICT_ID);
        }
    };
    Some(DiagnosticId::new("terminal", item))
}

fn probe_notes(snapshot: &DiagnosticSnapshot<'_>) -> Vec<ProbeNote> {
    let mut notes = Vec::new();
    if snapshot.common.terminal.is_tmux_backed() {
        probe_note(&mut notes, "tmux.version", &snapshot.common.tmux.version);
        probe_note(
            &mut notes,
            "tmux.extended-keys",
            &snapshot.common.tmux.extended_keys,
        );
        probe_note(
            &mut notes,
            "tmux.set-clipboard",
            &snapshot.common.tmux.set_clipboard,
        );
        probe_note(
            &mut notes,
            "tmux.allow-passthrough-support",
            &snapshot.common.tmux.allow_passthrough_support,
        );
        if matches!(
            snapshot.common.tmux.allow_passthrough_support,
            TmuxProbeResult::Available(())
        ) {
            probe_note(
                &mut notes,
                "tmux.allow-passthrough",
                &snapshot.common.tmux.allow_passthrough,
            );
        }
        probe_note(
            &mut notes,
            "tmux.control-mode",
            &snapshot.common.tmux.control_mode,
        );
        probe_note(
            &mut notes,
            "tmux.client-features",
            &snapshot.common.tmux.client_features,
        );
    }
    runtime_probe_note(
        &mut notes,
        "runtime.fullscreen-active",
        snapshot.runtime.fullscreen_active,
    );
    runtime_probe_note(
        &mut notes,
        "runtime.kitty-flags-pushed",
        snapshot.runtime.kitty_flags_pushed,
    );
    runtime_probe_note(&mut notes, "runtime.xtversion", snapshot.runtime.xtversion);
    runtime_probe_note(&mut notes, "terminal.color", snapshot.color_level);
    if snapshot.common.wayland.is_wayland {
        probe_note(
            &mut notes,
            "wayland.data-control",
            &snapshot.common.wayland.data_control,
        );
    }
    notes
}

fn tmux_option_fact(result: &TmuxProbeResult<String>) -> TmuxOptionFact {
    match result {
        TmuxProbeResult::Available(value) => TmuxOptionFact::Available(value.to_owned()),
        TmuxProbeResult::Unsupported => TmuxOptionFact::Unsupported,
        TmuxProbeResult::Unavailable => TmuxOptionFact::Unavailable,
        TmuxProbeResult::Error(_) => TmuxOptionFact::Error,
    }
}

/// tmux marks a client `RGB` when the outer terminfo declares `RGB`/`Tc` or
/// `terminal-features` adds it; either way the feature list is the single
/// authoritative signal, and a missing answer is not evidence of clamping.
fn tmux_color_passthrough(result: &TmuxProbeResult<String>) -> TmuxColorPassthrough {
    let TmuxProbeResult::Available(features) = result else {
        return TmuxColorPassthrough::Unknown;
    };
    if features.trim().is_empty() {
        return TmuxColorPassthrough::Unknown;
    }
    if features
        .split(',')
        .any(|feature| feature.trim().eq_ignore_ascii_case("RGB"))
    {
        TmuxColorPassthrough::Forwarded
    } else {
        TmuxColorPassthrough::Reduced
    }
}

fn tmux_support_fact(result: &TmuxProbeResult<()>) -> TmuxSupportFact {
    match result {
        TmuxProbeResult::Available(()) => TmuxSupportFact::Supported,
        TmuxProbeResult::Unsupported => TmuxSupportFact::Unsupported,
        TmuxProbeResult::Unavailable => TmuxSupportFact::Unavailable,
        TmuxProbeResult::Error(_) => TmuxSupportFact::Error,
    }
}

fn probe_note<T>(notes: &mut Vec<ProbeNote>, probe: &'static str, result: &TmuxProbeResult<T>) {
    let (status, message) = match result {
        TmuxProbeResult::Available(_) => return,
        TmuxProbeResult::Unsupported => (ProbeStatus::Unsupported, None),
        TmuxProbeResult::Unavailable => (ProbeStatus::Unavailable, None),
        TmuxProbeResult::Error(error) => (ProbeStatus::Error, Some(error.to_owned())),
    };
    notes.push(ProbeNote {
        probe,
        status,
        message,
    });
}

fn runtime_probe_note<T>(
    notes: &mut Vec<ProbeNote>,
    probe: &'static str,
    evidence: RuntimeEvidence<T>,
) {
    if matches!(evidence, RuntimeEvidence::Unavailable) {
        notes.push(ProbeNote {
            probe,
            status: ProbeStatus::Unavailable,
            message: None,
        });
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
