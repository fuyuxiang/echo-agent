//! Shared terminal diagnostic report types.

use crate::clipboard::{ClipboardDelivery, NativeClipboardPreflight, Osc52Capability};
use crate::host::{DisplayServer, HostOs};
use crate::terminal::{ByobuBackend, ModifierDelivery, MultiplexerKind, TerminalName};
use crate::theme::ThemeKind;
use crate::theme::color_support::ColorLevel;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeFact<T> {
    Available(T),
    NoReply,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DiagnosticId {
    pub domain: &'static str,
    pub item: &'static str,
}

impl DiagnosticId {
    pub const fn new(domain: &'static str, item: &'static str) -> Self {
        Self { domain, item }
    }
}

impl std::fmt::Display for DiagnosticId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.domain, self.item)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticReport {
    pub facts: DiagnosticFacts,
    pub findings: Vec<DiagnosticFinding>,
    pub probe_notes: Vec<ProbeNote>,
}

pub(crate) const NOTIFICATION_PROTOCOL_FALLBACK_ID: DiagnosticId =
    DiagnosticId::new("notifications", "protocol-fallback");
pub(crate) const FOCUS_TRACKING_UNAVAILABLE_ID: DiagnosticId =
    DiagnosticId::new("notifications", "focus-tracking-unavailable");
pub(crate) const SANDBOX_PROFILE_CONFLICT_ID: DiagnosticId =
    DiagnosticId::new("sandbox", "profile-conflict");
pub(crate) const CLIPBOARD_DELIVERY_UNVERIFIED_ID: DiagnosticId =
    DiagnosticId::new("clipboard", "delivery-unverified");
pub(crate) const CLIPBOARD_DELIVERY_UNAVAILABLE_ID: DiagnosticId =
    DiagnosticId::new("clipboard", "delivery-unavailable");
pub(crate) const NEWLINE_FALLBACK_ID: DiagnosticId =
    DiagnosticId::new("terminal", "newline-fallback");
pub(crate) const ITERM2_CLIPBOARD_PERMISSION_ID: DiagnosticId =
    DiagnosticId::new("terminal", "iterm2-clipboard-permission");
pub(crate) const VSCODE_SSH_NON_ASCII_ID: DiagnosticId =
    DiagnosticId::new("clipboard", "vscode-ssh-non-ascii");
pub(crate) const VOICE_NO_INPUT_DEVICE_ID: DiagnosticId =
    DiagnosticId::new("voice", "no-input-device");

impl DiagnosticReport {
    pub fn issue_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Issue)
            .count()
            + usize::from(
                !self.facts.clipboard.delivery.is_confirmed()
                    && !self.findings.iter().any(|finding| {
                        matches!(
                            finding.id,
                            CLIPBOARD_DELIVERY_UNVERIFIED_ID | CLIPBOARD_DELIVERY_UNAVAILABLE_ID
                        )
                    }),
            )
    }

    pub fn recommendation_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Recommendation)
            .count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticFacts {
    pub terminal: TerminalName,
    pub xtversion: RuntimeFact<String>,
    pub multiplexer: MultiplexerKind,
    pub byobu: Option<ByobuBackend>,
    pub ssh: bool,
    pub tmux: TmuxFacts,
    pub color: ColorFacts,
    pub keyboard: Option<KeyboardFact>,
    pub newline: Option<NewlineFact>,
    pub clipboard: ClipboardFacts,
    /// Passive mic enumeration when voice capture is available. `None` omits the
    /// Voice section (no-audio builds, or TUI when voice mode is off).
    pub voice: Option<VoiceFacts>,
}

/// Result of a passive input-device lookup (does not open a capture stream).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoiceFacts {
    /// Device (or Linux recorder) capture would open.
    Device { name: String, detail: String },
    /// Audio is compiled in but no default input / recorder exists.
    Missing { error: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TmuxFacts {
    pub extended_keys: TmuxOptionFact,
    pub set_clipboard: TmuxOptionFact,
    pub allow_passthrough_support: TmuxSupportFact,
    pub allow_passthrough: TmuxOptionFact,
    pub color_passthrough: TmuxColorPassthrough,
}

/// Whether the attached tmux client forwards 24-bit color to the terminal.
///
/// tmux resolves a client's features once, at attach time, so this describes
/// the live client and not the config on disk: a config change applies only
/// after that client reattaches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TmuxColorPassthrough {
    /// The client advertises `RGB`, so truecolor SGR reaches the terminal.
    Forwarded,
    /// tmux reduces 24-bit color to the client terminfo's palette, which is
    /// what makes themes look washed out even when EchoAgent emits truecolor.
    Reduced,
    /// No usable evidence: tmux predates `terminal-features` (3.2), no client
    /// is attached, or the query failed. Never treated as a problem.
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TmuxOptionFact {
    Available(String),
    Unsupported,
    Unavailable,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TmuxSupportFact {
    Supported,
    Unsupported,
    Unavailable,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColorFacts {
    pub level: RuntimeFact<ColorLevel>,
    pub available_themes: Vec<ThemeKind>,
    pub total_themes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardFact {
    pub modifier_delivery: ModifierDelivery,
    pub os: HostOs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NewlineFact {
    Vte { version: Option<String> },
    XtermJs { terminal: TerminalName },
    NoKittyKeyboardProtocol,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardFacts {
    pub native_route: bool,
    pub native_tool: String,
    pub native_preflight: NativeClipboardPreflight,
    pub tmux_route: bool,
    pub osc52_route: bool,
    pub osc52_capability: Osc52Capability,
    pub wrap_sink: bool,
    pub display_server: DisplayServer,
    pub container_no_display: bool,
    pub data_control: DataControlFact,
    pub delivery: ClipboardDelivery,
    /// Compatibility projection for compact status/JSON consumers. Detailed
    /// policy and remediation live in named findings.
    pub fix: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataControlFact {
    Available,
    Missing,
    Unavailable,
    Error,
    NotApplicable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticFinding {
    pub id: DiagnosticId,
    pub disposition: FindingDisposition,
    pub message: String,
    pub remediation: Option<ManualRemediation>,
    pub automatic_remediation: Option<crate::diagnostics::AutomaticRemediation>,
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindingDisposition {
    Issue,
    Recommendation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManualRemediation {
    pub fix: String,
    pub config_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeNote {
    pub probe: &'static str,
    pub status: ProbeStatus,
    pub message: Option<String>,
}

pub(crate) fn probe_requires_live_tui(note: &ProbeNote) -> bool {
    note.status == ProbeStatus::Unavailable
        && matches!(
            note.probe,
            "runtime.fullscreen-active" | "runtime.kitty-flags-pushed" | "runtime.xtversion"
        )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeStatus {
    Unsupported,
    Unavailable,
    Error,
}
