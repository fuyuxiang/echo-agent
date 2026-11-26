use serde::Serialize;

use crate::clipboard::{ClipboardDelivery, NativeClipboardPreflight, Osc52Capability};
use crate::diagnostics::{
    DataControlFact, DiagnosticFinding, DiagnosticReport, FindingDisposition, NewlineFact,
    ProbeNote, ProbeStatus, RuntimeFact, VoiceFacts,
};
use crate::host::HostOs;
use crate::terminal::{ByobuBackend, ModifierFate, MultiplexerKind, TerminalName};
use crate::theme::color_support::ColorLevel;

use super::SCHEMA_VERSION;

pub(super) fn write(
    report: &DiagnosticReport,
    writer: &mut impl std::io::Write,
) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(&mut *writer, &JsonReport::from(report))?;
    writeln!(writer)?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonReport<'a> {
    schema_version: &'static str,
    facts: JsonFacts<'a>,
    findings: Vec<JsonFinding<'a>>,
    probe_notes: Vec<JsonProbeNote<'a>>,
    counts: JsonCounts,
}

impl<'a> From<&'a DiagnosticReport> for JsonReport<'a> {
    fn from(report: &'a DiagnosticReport) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            facts: JsonFacts::from(report),
            findings: report.findings.iter().map(JsonFinding::from).collect(),
            probe_notes: report.probe_notes.iter().map(JsonProbeNote::from).collect(),
            counts: JsonCounts {
                issues: report.issue_count(),
                recommendations: report.recommendation_count(),
                probe_notes: report.probe_notes.len(),
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonFacts<'a> {
    terminal: JsonTerminalFact<'a>,
    multiplexer: JsonMultiplexerFact,
    ssh: bool,
    color: JsonColorFacts,
    keyboard: Option<JsonKeyboardFact>,
    newline: Option<JsonNewlineFact<'a>>,
    clipboard: JsonClipboardFacts<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice: Option<JsonVoiceFacts<'a>>,
}

impl<'a> From<&'a DiagnosticReport> for JsonFacts<'a> {
    fn from(report: &'a DiagnosticReport) -> Self {
        let facts = &report.facts;
        Self {
            terminal: JsonTerminalFact {
                name: terminal_name(facts.terminal),
                xtversion: JsonRuntimeFact::from(&facts.xtversion),
            },
            multiplexer: JsonMultiplexerFact {
                kind: multiplexer(facts.multiplexer),
                byobu: facts.byobu.map(byobu_backend),
            },
            ssh: facts.ssh,
            color: JsonColorFacts {
                level: JsonColorLevel::from(&facts.color.level),
                available_themes: facts
                    .color
                    .available_themes
                    .iter()
                    .map(|theme| theme.display_name())
                    .collect(),
                total_themes: facts.color.total_themes,
            },
            keyboard: facts.keyboard.as_ref().map(|keyboard| JsonKeyboardFact {
                cmd: modifier_fate(keyboard.modifier_delivery.cmd),
                opt: modifier_fate(keyboard.modifier_delivery.opt),
                os: host_os(keyboard.os),
            }),
            newline: facts.newline.as_ref().map(JsonNewlineFact::from),
            clipboard: JsonClipboardFacts::from(&facts.clipboard),
            voice: facts.voice.as_ref().map(JsonVoiceFacts::from),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonVoiceFacts<'a> {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

impl<'a> From<&'a VoiceFacts> for JsonVoiceFacts<'a> {
    fn from(facts: &'a VoiceFacts) -> Self {
        match facts {
            VoiceFacts::Device { name, detail } => Self {
                status: "available",
                name: Some(name),
                detail: Some(detail),
                error: None,
            },
            VoiceFacts::Missing { error } => Self {
                status: "missing",
                name: None,
                detail: None,
                error: Some(error),
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonTerminalFact<'a> {
    name: &'static str,
    xtversion: JsonRuntimeFact<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonMultiplexerFact {
    kind: &'static str,
    byobu: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonRuntimeFact<'a> {
    status: &'static str,
    value: Option<&'a str>,
}

impl<'a> From<&'a RuntimeFact<String>> for JsonRuntimeFact<'a> {
    fn from(fact: &'a RuntimeFact<String>) -> Self {
        match fact {
            RuntimeFact::Available(value) => Self {
                status: "available",
                value: Some(value),
            },
            RuntimeFact::NoReply => Self {
                status: "no_reply",
                value: None,
            },
            RuntimeFact::Unavailable => Self {
                status: "unavailable",
                value: None,
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonColorFacts {
    level: JsonColorLevel,
    available_themes: Vec<&'static str>,
    total_themes: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonColorLevel {
    status: &'static str,
    value: Option<&'static str>,
}

impl From<&RuntimeFact<ColorLevel>> for JsonColorLevel {
    fn from(fact: &RuntimeFact<ColorLevel>) -> Self {
        match fact {
            RuntimeFact::Available(level) => Self {
                status: "available",
                value: Some(level.as_str()),
            },
            RuntimeFact::NoReply => Self {
                status: "no_reply",
                value: None,
            },
            RuntimeFact::Unavailable => Self {
                status: "unavailable",
                value: None,
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonKeyboardFact {
    cmd: &'static str,
    opt: &'static str,
    os: &'static str,
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum JsonNewlineFact<'a> {
    Vte { version: Option<&'a str> },
    XtermJs { terminal_name: &'static str },
    NoKittyKeyboardProtocol,
}

impl<'a> From<&'a NewlineFact> for JsonNewlineFact<'a> {
    fn from(newline: &'a NewlineFact) -> Self {
        match newline {
            NewlineFact::Vte { version } => Self::Vte {
                version: version.as_deref(),
            },
            NewlineFact::XtermJs { terminal } => Self::XtermJs {
                terminal_name: terminal_name(*terminal),
            },
            NewlineFact::NoKittyKeyboardProtocol => Self::NoKittyKeyboardProtocol,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonClipboardFacts<'a> {
    native_route: bool,
    native_tool: &'a str,
    native_preflight: &'static str,
    tmux_route: bool,
    osc52_route: bool,
    osc52_capability: &'static str,
    wrap_sink: bool,
    display_server: &'static str,
    container_no_display: bool,
    data_control: &'static str,
    delivery: &'static str,
    fix: Option<&'a str>,
}

impl<'a> From<&'a crate::diagnostics::ClipboardFacts> for JsonClipboardFacts<'a> {
    fn from(facts: &'a crate::diagnostics::ClipboardFacts) -> Self {
        Self {
            native_route: facts.native_route,
            native_tool: &facts.native_tool,
            native_preflight: native_preflight(facts.native_preflight),
            tmux_route: facts.tmux_route,
            osc52_route: facts.osc52_route,
            osc52_capability: osc52_capability(facts.osc52_capability),
            wrap_sink: facts.wrap_sink,
            display_server: display_server(facts.display_server),
            container_no_display: facts.container_no_display,
            data_control: data_control(facts.data_control),
            delivery: clipboard_delivery(facts.delivery),
            fix: facts.fix.as_deref(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonFinding<'a> {
    id: String,
    disposition: &'static str,
    message: &'a str,
    remediation: Option<JsonRemediation<'a>>,
    automatic_remediation: Option<JsonAutomaticRemediation>,
    note: Option<&'a str>,
}

impl<'a> From<&'a DiagnosticFinding> for JsonFinding<'a> {
    fn from(finding: &'a DiagnosticFinding) -> Self {
        Self {
            id: finding.id.to_string(),
            disposition: match finding.disposition {
                FindingDisposition::Issue => "issue",
                FindingDisposition::Recommendation => "recommendation",
            },
            message: &finding.message,
            remediation: finding
                .remediation
                .as_ref()
                .map(|remediation| JsonRemediation {
                    fix: &remediation.fix,
                    config_path: remediation.config_path.as_deref(),
                }),
            automatic_remediation: finding.automatic_remediation.map(|automatic| {
                JsonAutomaticRemediation {
                    fix_id: automatic.fix_id.to_string(),
                    command: automatic.command,
                }
            }),
            note: finding.note.as_deref(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonRemediation<'a> {
    fix: &'a str,
    config_path: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonAutomaticRemediation {
    fix_id: String,
    command: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonProbeNote<'a> {
    probe: &'static str,
    status: &'static str,
    message: Option<&'a str>,
}

impl<'a> From<&'a ProbeNote> for JsonProbeNote<'a> {
    fn from(note: &'a ProbeNote) -> Self {
        Self {
            probe: note.probe,
            status: probe_status(note.status),
            message: note.message.as_deref(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonCounts {
    issues: usize,
    recommendations: usize,
    probe_notes: usize,
}

pub(super) fn terminal_name(name: TerminalName) -> &'static str {
    match name {
        TerminalName::AppleTerminal => "apple_terminal",
        TerminalName::Ghostty => "ghostty",
        TerminalName::Iterm2 => "iterm2",
        TerminalName::WarpTerminal => "warp",
        TerminalName::VsCode => "vs_code",
        TerminalName::Cursor => "cursor",
        TerminalName::Windsurf => "windsurf",
        TerminalName::Zed => "zed",
        TerminalName::WezTerm => "wezterm",
        TerminalName::Kitty => "kitty",
        TerminalName::Alacritty => "alacritty",
        TerminalName::Rio => "rio",
        TerminalName::Foot => "foot",
        TerminalName::JetBrains => "jetbrains",
        TerminalName::EchoAgentDesktop => "echo_agent_desktop",
        TerminalName::Vte => "vte",
        TerminalName::Terminator => "terminator",
        TerminalName::WindowsTerminal => "windows_terminal",
        TerminalName::Otty => "otty",
        TerminalName::Unknown => "unknown",
    }
}

pub(super) fn multiplexer(kind: MultiplexerKind) -> &'static str {
    match kind {
        MultiplexerKind::Tmux => "tmux",
        MultiplexerKind::Screen => "screen",
        MultiplexerKind::Zellij => "zellij",
        MultiplexerKind::Cmux => "cmux",
        MultiplexerKind::Herdr => "herdr",
        MultiplexerKind::Undetected => "undetected",
    }
}

pub(super) fn byobu_backend(backend: ByobuBackend) -> &'static str {
    match backend {
        ByobuBackend::Unknown => "unknown",
        ByobuBackend::Tmux => "tmux",
        ByobuBackend::Screen => "screen",
    }
}

pub(super) fn modifier_fate(fate: ModifierFate) -> &'static str {
    match fate {
        ModifierFate::Native => "native",
        ModifierFate::Dropped => "dropped",
        ModifierFate::Unrecoverable => "unrecoverable",
        ModifierFate::Unknown => "unknown",
        _ => "unknown",
    }
}

pub(super) fn host_os(os: HostOs) -> &'static str {
    match os {
        HostOs::Macos => "macos",
        HostOs::Linux => "linux",
        HostOs::Windows => "windows",
        HostOs::Other => "other",
        _ => "other",
    }
}

pub(super) fn native_preflight(fact: NativeClipboardPreflight) -> &'static str {
    match fact {
        NativeClipboardPreflight::Disabled => "disabled",
        NativeClipboardPreflight::LocalAvailable => "local_available",
        NativeClipboardPreflight::RemoteOnly => "remote_only",
        NativeClipboardPreflight::Unavailable => "unavailable",
    }
}

pub(super) fn osc52_capability(capability: Osc52Capability) -> &'static str {
    match capability {
        Osc52Capability::Supported => "supported",
        Osc52Capability::Unsupported => "unsupported",
        Osc52Capability::Unknown => "unknown",
    }
}

pub(super) fn display_server(server: crate::host::DisplayServer) -> &'static str {
    match server {
        crate::host::DisplayServer::Quartz => "quartz",
        crate::host::DisplayServer::Wayland => "wayland",
        crate::host::DisplayServer::X11 => "x11",
        crate::host::DisplayServer::Win32 => "win32",
        crate::host::DisplayServer::Unknown => "unknown",
        _ => "unknown",
    }
}

pub(super) fn clipboard_delivery(delivery: ClipboardDelivery) -> &'static str {
    match delivery {
        ClipboardDelivery::Confirmed => "confirmed",
        ClipboardDelivery::Unverified => "unverified",
        ClipboardDelivery::Failed => "failed",
    }
}

pub(super) fn data_control(fact: DataControlFact) -> &'static str {
    match fact {
        DataControlFact::Available => "available",
        DataControlFact::Missing => "missing",
        DataControlFact::Unavailable => "unavailable",
        DataControlFact::Error => "error",
        DataControlFact::NotApplicable => "not_applicable",
    }
}

pub(super) fn probe_status(status: ProbeStatus) -> &'static str {
    match status {
        ProbeStatus::Unsupported => "unsupported",
        ProbeStatus::Unavailable => "unavailable",
        ProbeStatus::Error => "error",
    }
}
