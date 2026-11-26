//! Exact planning and application for diagnostic fixes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use echo_agent_config::managed_text::{
    CommentSyntax, ManagedConfig, ManagedConfigOutcome, ManagedConfigPlan, ManagedConfigRequest,
    ManagedConfigStatus, ManagedItem, ManagedItemState, SyntaxValidator,
};

use crate::diagnostics::{
    DiagnosticId, DiagnosticReport, TmuxColorPassthrough, TmuxOptionFact, TmuxSupportFact,
};
use crate::terminal::{ByobuBackend, TerminalContext};

pub const SSH_WRAP_ID: DiagnosticId = DiagnosticId::new("terminal", "ssh-wrap");
pub const TMUX_CLIPBOARD_ID: DiagnosticId = DiagnosticId::new("terminal", "tmux-clipboard");
pub const DCS_PASSTHROUGH_ID: DiagnosticId = DiagnosticId::new("terminal", "dcs-passthrough");
pub const TMUX_EXTENDED_KEYS_ID: DiagnosticId = DiagnosticId::new("terminal", "tmux-extended-keys");
pub const TMUX_TRUECOLOR_ID: DiagnosticId = DiagnosticId::new("terminal", "tmux-truecolor");
pub const SSH_WRAP_FIX_COMMAND: &str = "echoagent doctor fix terminal.ssh-wrap";
pub const SSH_WRAP_ONE_OFF: &str = "echoagent wrap ssh <host>";

const MANAGED_NAMESPACE: &str = "echoagent doctor";
const SSH_WRAP_ALIAS_POSIX: &str = "alias ssh='echoagent wrap ssh'";
const SSH_WRAP_ALIAS_FISH: &str = "alias ssh 'echoagent wrap ssh'";
const TMUX_SCANNER_CAVEAT: &str = "EchoAgent checks this file for direct global assignments of this option. Review sourced files, conditionals, plugins, and generated tmux setup yourself.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutomaticRemediation {
    pub fix_id: DiagnosticId,
    pub command: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixRequest {
    id: DiagnosticId,
    home: SafeAbsoluteDirectory,
    shell: Option<PathBuf>,
    validator: Option<PathBuf>,
    byobu_config_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SafeAbsoluteDirectory(PathBuf);

impl SafeAbsoluteDirectory {
    fn parse(path: PathBuf, label: &'static str) -> Result<Self, FixError> {
        use std::path::Component;

        let is_root_only = path.parent().is_none();
        let has_unsafe_component = path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir));
        let is_renderable = path
            .to_str()
            .is_some_and(|value| !value.chars().any(char::is_control) && !value.contains('~'));
        if !path.is_absolute() || is_root_only || has_unsafe_component || !is_renderable {
            return Err(FixError::UnsafeDirectory { label, path });
        }
        Ok(Self(path))
    }

    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl FixRequest {
    #[cfg(test)]
    pub(crate) fn new_for_test(
        id: DiagnosticId,
        home: &Path,
        shell: Option<PathBuf>,
        validator: Option<PathBuf>,
        byobu_config_dir: Option<PathBuf>,
    ) -> Result<Self, FixError> {
        Ok(Self {
            id,
            home: SafeAbsoluteDirectory::parse(home.to_path_buf(), "HOME")?,
            shell,
            validator,
            byobu_config_dir,
        })
    }

    pub fn from_environment(id: DiagnosticId) -> Result<Self, FixError> {
        let home =
            SafeAbsoluteDirectory::parse(actual_home().ok_or(FixError::HomeUnavailable)?, "HOME")?;
        let shell = std::env::var_os("SHELL").map(PathBuf::from);
        let validator = shell.as_deref().and_then(resolve_validator_program);
        let byobu_config_dir = std::env::var_os("BYOBU_CONFIG_DIR").map(PathBuf::from);
        Ok(Self {
            id,
            home,
            shell,
            validator,
            byobu_config_dir,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
}

impl ShellKind {
    pub fn from_shell_path(shell: &Path) -> Option<Self> {
        match shell.file_name()?.to_str()? {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }

    pub(crate) fn config_path(self, home: &Path) -> PathBuf {
        match self {
            Self::Bash => home.join(".bashrc"),
            Self::Zsh => home.join(".zshrc"),
            Self::Fish => home.join(".config/fish/config.fish"),
        }
    }

    fn alias(self) -> &'static str {
        match self {
            Self::Bash | Self::Zsh => SSH_WRAP_ALIAS_POSIX,
            Self::Fish => SSH_WRAP_ALIAS_FISH,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedChange {
    pub requested_path: PathBuf,
    pub target_path: PathBuf,
    pub block: String,
    pub backup_path_hint: Option<PathBuf>,
    pub will_write: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixActivation {
    SatisfiedNow,
    RequiresReload,
}

#[derive(Clone, Debug)]
pub struct FixPlan {
    id: DiagnosticId,
    change: PlannedChange,
    caveats: Vec<&'static str>,
    payload: FixPayload,
}

impl FixPlan {
    pub fn id(&self) -> DiagnosticId {
        self.id
    }

    pub fn change(&self) -> &PlannedChange {
        &self.change
    }

    pub fn caveats(&self) -> &[&'static str] {
        &self.caveats
    }
}

#[derive(Clone, Debug)]
enum FixPayload {
    SshWrap(SshWrapPlan),
    TmuxOption(TmuxOptionPlan),
}

#[derive(Clone, Debug)]
struct SshWrapPlan {
    shell: ShellKind,
    managed: ManagedConfigPlan,
}

#[derive(Clone, Debug)]
struct TmuxOptionPlan {
    spec: &'static TmuxOptionSpec,
    managed: ManagedConfigPlan,
    direct_state: DirectOptionState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixStatus {
    Applied,
    AlreadyConfigured,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChangedFile {
    path: PathBuf,
    backup_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixOutcome {
    id: DiagnosticId,
    status: FixStatus,
    changed_file: ChangedFile,
    activation: FixActivation,
    /// Shell used to plan/apply SSH-wrap. Post-apply verification must use this
    /// rather than re-reading `$SHELL`, which may be missing or different.
    shell: Option<ShellKind>,
}

impl FixOutcome {
    #[cfg(test)]
    pub(crate) fn new_for_test(
        id: DiagnosticId,
        status: FixStatus,
        path: PathBuf,
        backup_path: Option<PathBuf>,
        activation: FixActivation,
        shell: Option<ShellKind>,
    ) -> Self {
        Self::new(
            id,
            status,
            ChangedFile { path, backup_path },
            activation,
            shell,
        )
    }

    fn new(
        id: DiagnosticId,
        status: FixStatus,
        changed_file: ChangedFile,
        activation: FixActivation,
        shell: Option<ShellKind>,
    ) -> Self {
        Self {
            id,
            status,
            changed_file,
            activation,
            shell,
        }
    }

    pub fn id(&self) -> DiagnosticId {
        self.id
    }

    pub fn status(&self) -> FixStatus {
        self.status
    }

    pub fn activation(&self) -> FixActivation {
        self.activation
    }

    pub fn changed_path(&self) -> &Path {
        &self.changed_file.path
    }

    pub fn backup_path(&self) -> Option<&Path> {
        self.changed_file.backup_path.as_deref()
    }

    /// Shell that planned and applied this fix, when the fix is shell-scoped.
    pub fn shell(&self) -> Option<ShellKind> {
        self.shell
    }

    /// Whether the SSH-wrap managed alias is present for the shell that applied
    /// this outcome. Uses the planned shell, not the current `$SHELL`.
    pub fn managed_alias_is_configured(&self) -> bool {
        self.shell
            .is_some_and(|shell| managed_alias_configured(&self.changed_file.path, shell))
    }
}

#[derive(Debug)]
pub enum FixError {
    UnknownId(String),
    PlatformUnsupported,
    HomeUnavailable,
    NotApplicable,
    TmuxNotApplicable,
    RemoteSession,
    UnsupportedShell,
    ByobuConfigUnavailable,
    UnsafeDirectory { label: &'static str, path: PathBuf },
    ExistingCustomization { path: PathBuf, detail: String },
    Managed(echo_agent_config::managed_text::ManagedConfigError),
    TmuxManaged(echo_agent_config::managed_text::ManagedConfigError),
    PostconditionFailed,
    TmuxPostconditionFailed,
}

impl std::fmt::Display for FixError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownId(id) => write!(
                formatter,
                "`{id}` is not an available Doctor fix. Run `echoagent doctor fix` to list available fixes."
            ),
            Self::PlatformUnsupported => write!(
                formatter,
                "Automatic SSH setup is not available on Windows. Run `{SSH_WRAP_ONE_OFF}` when needed."
            ),
            Self::HomeUnavailable => formatter.write_str("EchoAgent could not find your home directory."),
            Self::NotApplicable => formatter
                .write_str("This fix does not apply to VS Code Remote sessions."),
            Self::TmuxNotApplicable => formatter
                .write_str("This fix is not applicable to the current report."),
            Self::RemoteSession => formatter
                .write_str("Run this fix on your local computer, not in the SSH session."),
            Self::UnsupportedShell => write!(
                formatter,
                "Automatic setup supports Bash, zsh, and fish. For another shell, run `{SSH_WRAP_ONE_OFF}` when needed."
            ),
            Self::ByobuConfigUnavailable => formatter.write_str(
                "EchoAgent could not determine Byobu's effective config directory. Keep `BYOBU_CONFIG_DIR` set in this session, then run the fix again.",
            ),
            Self::UnsafeDirectory { label, path } => write!(
                formatter,
                "EchoAgent refused unsafe {label} `{}`. Use a non-root absolute directory without control characters, `~`, `.` or `..` components.",
                path.display()
            ),
            Self::ExistingCustomization { path, detail }
                if detail.starts_with("existing `alias ssh")
                    || detail.contains("`ssh` fish function") =>
            {
                write!(
                    formatter,
                    "EchoAgent found an existing SSH alias or function in {} and did not change it: {detail}",
                    path.display()
                )
            }
            Self::ExistingCustomization { path, detail } => write!(
                formatter,
                "EchoAgent found an existing customization in {} and did not change it: {detail}",
                path.display()
            ),
            Self::Managed(error) => write!(
                formatter,
                "Could not update your shell configuration: {error}"
            ),
            Self::TmuxManaged(error) => {
                write!(formatter, "Could not update your tmux configuration: {error}")
            }
            Self::PostconditionFailed => formatter
                .write_str("The configuration changed, but EchoAgent could not verify the SSH alias."),
            Self::TmuxPostconditionFailed => formatter.write_str(
                "The configuration changed, but EchoAgent could not verify the managed tmux option.",
            ),
        }
    }
}

impl std::error::Error for FixError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Managed(error) | Self::TmuxManaged(error) => Some(error),
            _ => None,
        }
    }
}

impl From<echo_agent_config::managed_text::ManagedConfigError> for FixError {
    fn from(error: echo_agent_config::managed_text::ManagedConfigError) -> Self {
        Self::Managed(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AutomaticFixAvailability {
    Here,
    RunLocally,
}

#[derive(Clone, Copy)]
enum FixKind {
    SshWrap,
    TmuxOption(&'static TmuxOptionSpec),
}

#[derive(Clone, Copy)]
struct FixSpec {
    id: DiagnosticId,
    handle: &'static str,
    label: &'static str,
    command: &'static str,
    kind: FixKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TmuxEvidence {
    Clipboard,
    DcsPassthrough,
    ExtendedKeys,
    ColorPassthrough,
}

/// How a tmux remedy reaches its healthy state, which decides whether an
/// existing line elsewhere in the config can defeat EchoAgent's managed block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TmuxRemedy {
    /// `set -g <option> <value>`: the last assignment wins, so a direct
    /// assignment in the user's own config must be classified before writing.
    Assignment,
    /// `set -as <option> …`: tmux accumulates these and EchoAgent appends its block
    /// at the end of the file, so earlier lines add to the fix rather than
    /// override it and are never a conflict.
    Accumulating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TmuxOptionSpec {
    id: DiagnosticId,
    option: &'static str,
    line: &'static str,
    /// Values that already satisfy the fix. Empty for an accumulating remedy,
    /// whose health comes from the attached client, not from one option value.
    healthy_values: &'static [&'static str],
    remedy: TmuxRemedy,
    evidence: TmuxEvidence,
    scope: TmuxOptionScope,
    label: &'static str,
}

const TMUX_CLIPBOARD_SPEC: TmuxOptionSpec = TmuxOptionSpec {
    id: TMUX_CLIPBOARD_ID,
    option: "set-clipboard",
    line: "set -g set-clipboard on",
    healthy_values: &["on", "external"],
    remedy: TmuxRemedy::Assignment,
    evidence: TmuxEvidence::Clipboard,
    scope: TmuxOptionScope::Server,
    label: "Enable tmux clipboard forwarding",
};
const DCS_PASSTHROUGH_SPEC: TmuxOptionSpec = TmuxOptionSpec {
    id: DCS_PASSTHROUGH_ID,
    option: "allow-passthrough",
    line: "set -wg allow-passthrough on",
    healthy_values: &["on", "all"],
    remedy: TmuxRemedy::Assignment,
    evidence: TmuxEvidence::DcsPassthrough,
    scope: TmuxOptionScope::Window,
    label: "Enable tmux DCS passthrough",
};
const TMUX_EXTENDED_KEYS_SPEC: TmuxOptionSpec = TmuxOptionSpec {
    id: TMUX_EXTENDED_KEYS_ID,
    option: "extended-keys",
    line: "set -g extended-keys on",
    healthy_values: &["on"],
    remedy: TmuxRemedy::Assignment,
    evidence: TmuxEvidence::ExtendedKeys,
    scope: TmuxOptionScope::Server,
    label: "Enable tmux extended keys",
};

const TMUX_TRUECOLOR_SPEC: TmuxOptionSpec = TmuxOptionSpec {
    id: TMUX_TRUECOLOR_ID,
    option: "terminal-features",
    line: "set -as terminal-features \",*:RGB\"",
    healthy_values: &[],
    remedy: TmuxRemedy::Accumulating,
    evidence: TmuxEvidence::ColorPassthrough,
    scope: TmuxOptionScope::Server,
    label: "Enable tmux truecolor passthrough",
};

const FIX_REGISTRY: &[FixSpec] = &[
    FixSpec {
        id: SSH_WRAP_ID,
        handle: "ssh-wrap",
        label: "Set up local SSH wrapping",
        command: SSH_WRAP_FIX_COMMAND,
        kind: FixKind::SshWrap,
    },
    FixSpec {
        id: TMUX_CLIPBOARD_ID,
        handle: "tmux-clipboard",
        label: TMUX_CLIPBOARD_SPEC.label,
        command: "echoagent doctor fix terminal.tmux-clipboard",
        kind: FixKind::TmuxOption(&TMUX_CLIPBOARD_SPEC),
    },
    FixSpec {
        id: DCS_PASSTHROUGH_ID,
        handle: "dcs-passthrough",
        label: DCS_PASSTHROUGH_SPEC.label,
        command: "echoagent doctor fix terminal.dcs-passthrough",
        kind: FixKind::TmuxOption(&DCS_PASSTHROUGH_SPEC),
    },
    FixSpec {
        id: TMUX_EXTENDED_KEYS_ID,
        handle: "tmux-extended-keys",
        label: TMUX_EXTENDED_KEYS_SPEC.label,
        command: "echoagent doctor fix terminal.tmux-extended-keys",
        kind: FixKind::TmuxOption(&TMUX_EXTENDED_KEYS_SPEC),
    },
    FixSpec {
        id: TMUX_TRUECOLOR_ID,
        handle: "tmux-truecolor",
        label: TMUX_TRUECOLOR_SPEC.label,
        command: "echoagent doctor fix terminal.tmux-truecolor",
        kind: FixKind::TmuxOption(&TMUX_TRUECOLOR_SPEC),
    },
];

fn fix_spec(id: DiagnosticId) -> Option<&'static FixSpec> {
    FIX_REGISTRY.iter().find(|spec| spec.id == id)
}

pub fn resolve_fix_id(value: &str) -> Result<DiagnosticId, FixError> {
    FIX_REGISTRY
        .iter()
        .find(|spec| value == spec.handle || value == spec.id.to_string())
        .map(|spec| spec.id)
        .ok_or_else(|| FixError::UnknownId(value.to_owned()))
}

pub(crate) fn human_fix_command(id: DiagnosticId) -> Option<String> {
    fix_spec(id).map(|spec| format!("echoagent doctor fix {}", spec.handle))
}

pub(crate) fn automatic_fix_choices()
-> impl Iterator<Item = (DiagnosticId, &'static str, &'static str)> {
    FIX_REGISTRY
        .iter()
        .map(|spec| (spec.id, spec.handle, spec.label))
}

pub(crate) fn automatic_remediation_for(id: DiagnosticId) -> Option<AutomaticRemediation> {
    fix_spec(id).map(|spec| AutomaticRemediation {
        fix_id: id,
        command: spec.command,
    })
}

pub fn ssh_wrap_automatic_remediation() -> AutomaticRemediation {
    automatic_remediation_for(SSH_WRAP_ID).expect("registered SSH wrap fix")
}

pub(crate) fn select_fix_plan(
    id: DiagnosticId,
    report: &DiagnosticReport,
    terminal: &TerminalContext,
) -> Result<Option<FixPlan>, FixError> {
    let spec = fix_spec(id).ok_or_else(|| FixError::UnknownId(id.to_string()))?;
    if matches!(spec.kind, FixKind::SshWrap)
        && (terminal.is_ssh || terminal.is_official_vscode_remote || report.facts.ssh)
    {
        return Ok(None);
    }
    plan_fix(FixRequest::from_environment(id)?, report, terminal).map(Some)
}

pub(crate) fn applicable_automatic_fixes(
    report: &DiagnosticReport,
    terminal: &TerminalContext,
) -> Vec<(DiagnosticId, &'static str, AutomaticFixAvailability)> {
    applicable_automatic_fixes_with(report, terminal, FixRequest::from_environment)
}

fn applicable_automatic_fixes_with(
    report: &DiagnosticReport,
    terminal: &TerminalContext,
    mut request_for: impl FnMut(DiagnosticId) -> Result<FixRequest, FixError>,
) -> Vec<(DiagnosticId, &'static str, AutomaticFixAvailability)> {
    report
        .findings
        .iter()
        .filter_map(|finding| {
            let automatic = finding.automatic_remediation?;
            let spec = fix_spec(automatic.fix_id)?;
            let availability = if matches!(spec.kind, FixKind::SshWrap)
                && (terminal.is_ssh || terminal.is_official_vscode_remote || report.facts.ssh)
            {
                AutomaticFixAvailability::RunLocally
            } else {
                plan_fix(request_for(automatic.fix_id).ok()?, report, terminal).ok()?;
                AutomaticFixAvailability::Here
            };
            Some((automatic.fix_id, spec.handle, availability))
        })
        .collect()
}

pub(crate) fn format_applicable_automatic_fixes(
    report: &DiagnosticReport,
    terminal: &TerminalContext,
) -> String {
    let fixes = applicable_automatic_fixes(report, terminal);
    if fixes.is_empty() {
        return "No automatic fixes are available here.\n".to_owned();
    }

    let mut output = String::from("Automatic fixes:\n");
    for (id, handle, availability) in fixes {
        let label = fix_spec(id).map_or("Apply automatic fix", |spec| spec.label);
        output.push_str(&format!("  {handle:<20} {label}\n"));
        match availability {
            AutomaticFixAvailability::Here => output.push_str(&format!(
                "    Run: echoagent doctor fix {handle}\n    In EchoAgent: /doctor fix {handle}\n"
            )),
            AutomaticFixAvailability::RunLocally => output.push_str(&format!(
                "    On your local computer, run: echoagent doctor fix {handle}\n"
            )),
        }
    }
    output
}

pub(crate) fn format_fix_preview(plan: &FixPlan) -> String {
    use std::fmt::Write as _;

    let mut output = String::from("Doctor Fix\n\n");
    let _ = writeln!(output, "Fix: {}", plan.id);
    if let FixPayload::SshWrap(payload) = &plan.payload {
        let _ = writeln!(output, "Shell: {}", payload.shell.name());
    }
    let change = &plan.change;
    let _ = writeln!(output, "File: {}", preview_path(&change.requested_path));
    if change.target_path != change.requested_path {
        let _ = writeln!(
            output,
            "Actual file: {} (symlink target)",
            preview_path(&change.target_path)
        );
    }
    if change.will_write {
        let _ = writeln!(output, "\nText to add:\n{}", change.block);
    } else {
        output.push_str("\nText to add: None. The requested setting is already configured.\n");
    }
    match &change.backup_path_hint {
        Some(path) => {
            let _ = writeln!(
                output,
                "\nBackup will be saved to: {}\nIf that file exists, EchoAgent will choose a unique name.",
                preview_path(path)
            );
        }
        None => output.push_str("\nBackup: None. The file is new or no changes are needed.\n"),
    }
    match &plan.payload {
        FixPayload::SshWrap(_) => {
            output.push_str(
                "\nWhat this changes:\n  In new interactive shells, `ssh ...` runs as `echoagent wrap ssh ...`.\n",
            );
            let _ = writeln!(
                output,
                "  To use once without changing config: `{SSH_WRAP_ONE_OFF}`."
            );
        }
        FixPayload::TmuxOption(payload) => {
            let instruction =
                tmux_activation_instruction(payload.spec, &plan.change.requested_path);
            let _ = writeln!(
                output,
                "\nWhat this changes:\n  Persists `{}`.\n  EchoAgent does not reload or modify the live tmux server.\n  After applying, {instruction}\n  Run /doctor again to verify the live setting.",
                payload.spec.line,
            );
        }
    }
    output.push_str("Caveats:\n");
    for caveat in &plan.caveats {
        let _ = writeln!(output, "  - {caveat}");
    }
    output
}

pub fn plan_fix(
    request: FixRequest,
    report: &DiagnosticReport,
    terminal: &TerminalContext,
) -> Result<FixPlan, FixError> {
    let spec = fix_spec(request.id).ok_or_else(|| FixError::UnknownId(request.id.to_string()))?;
    match spec.kind {
        FixKind::SshWrap => plan_ssh_wrap(request, report, terminal),
        FixKind::TmuxOption(tmux) => plan_tmux_option(request, report, terminal, tmux),
    }
}

fn plan_ssh_wrap(
    request: FixRequest,
    report: &DiagnosticReport,
    terminal: &TerminalContext,
) -> Result<FixPlan, FixError> {
    if cfg!(windows) {
        return Err(FixError::PlatformUnsupported);
    }
    if terminal.is_official_vscode_remote {
        return Err(FixError::NotApplicable);
    }
    if terminal.is_ssh || report.facts.ssh {
        return Err(FixError::RemoteSession);
    }

    let shell = request
        .shell
        .as_deref()
        .and_then(ShellKind::from_shell_path)
        .ok_or(FixError::UnsupportedShell)?;
    let managed = ManagedConfig::plan(ManagedConfigRequest {
        path: shell.config_path(&request.home.0),
        namespace: MANAGED_NAMESPACE.to_owned(),
        owned_item_prefix: "terminal.".to_owned(),
        items: vec![ManagedItem::new(request.id.to_string(), shell.alias())],
        comments: CommentSyntax::hash(),
        validator: validator_for(shell, request.validator),
    })?;
    if let Some(detail) = detect_ssh_customization(managed.inspection().unmanaged_text(), shell) {
        return Err(FixError::ExistingCustomization {
            path: managed.target_path().to_path_buf(),
            detail,
        });
    }
    let change = planned_change(&managed)?;
    Ok(FixPlan {
        id: request.id,
        change,
        caveats: vec![
            "The alias loads only in new interactive shells.",
            "Use `command ssh ...` to bypass the alias.",
            "For manually entered `ssh -f`, ControlPersist workflows, or OpenSSH `~^Z` local suspend, use `command ssh ...`. Wrapping does not fully preserve those behaviors.",
            "`echoagent wrap` starts the SSH process directly, so the alias does not loop.",
            "EchoAgent checks this file for direct SSH aliases and functions. Review sourced files, plugins, and generated shell setup yourself.",
        ],
        payload: FixPayload::SshWrap(SshWrapPlan { shell, managed }),
    })
}

fn plan_tmux_option(
    request: FixRequest,
    report: &DiagnosticReport,
    terminal: &TerminalContext,
    spec: &'static TmuxOptionSpec,
) -> Result<FixPlan, FixError> {
    if !terminal.is_tmux_backed()
        || terminal.byobu == Some(ByobuBackend::Screen)
        || report.facts.multiplexer != crate::terminal::MultiplexerKind::Tmux
        || !report.findings.iter().any(|finding| finding.id == spec.id)
        || !tmux_evidence_is_applicable(report, spec)
    {
        return Err(FixError::TmuxNotApplicable);
    }
    let managed = ManagedConfig::plan(ManagedConfigRequest {
        path: tmux_config_path(&request, terminal)?,
        namespace: MANAGED_NAMESPACE.to_owned(),
        owned_item_prefix: "terminal.".to_owned(),
        items: vec![ManagedItem::new(spec.id.to_string(), spec.line)],
        comments: CommentSyntax::hash(),
        validator: None,
    })
    .map_err(FixError::TmuxManaged)?;
    let direct = match spec.remedy {
        TmuxRemedy::Assignment => scan_direct_tmux_option(
            managed.inspection().unmanaged_text(),
            managed.target_path(),
            spec,
        )?,
        TmuxRemedy::Accumulating => DirectOptionState::Absent,
    };
    let item_state = managed
        .inspection()
        .requested_item_state(0)
        .ok_or(FixError::TmuxPostconditionFailed)?;
    let direct_noop = direct == DirectOptionState::Healthy
        && matches!(
            item_state,
            ManagedItemState::Absent | ManagedItemState::Exact
        );
    let mut change = planned_tmux_change(&managed)?;
    if direct_noop {
        change.will_write = false;
        change.backup_path_hint = None;
    }
    Ok(FixPlan {
        id: request.id,
        change,
        caveats: tmux_caveats(spec.remedy),
        payload: FixPayload::TmuxOption(TmuxOptionPlan {
            spec,
            managed,
            direct_state: if direct_noop {
                DirectOptionState::Healthy
            } else {
                DirectOptionState::Absent
            },
        }),
    })
}

fn tmux_caveats(remedy: TmuxRemedy) -> Vec<&'static str> {
    match remedy {
        TmuxRemedy::Assignment => vec![
            "The live tmux server is unchanged until you reload this config or restart it.",
            TMUX_SCANNER_CAVEAT,
        ],
        // Reloading is not enough on its own: tmux fixes a client's feature set
        // when that client attaches.
        TmuxRemedy::Accumulating => vec![
            "Reloading alone is not enough: the attached client keeps its current color depth until it reattaches.",
            "Terminals that cannot render 24-bit color ignore the extra escape sequence.",
        ],
    }
}

fn tmux_evidence_is_applicable(report: &DiagnosticReport, spec: &TmuxOptionSpec) -> bool {
    match spec.evidence {
        TmuxEvidence::ColorPassthrough => {
            report.facts.tmux.color_passthrough == TmuxColorPassthrough::Reduced
        }
        TmuxEvidence::Clipboard => matches!(
            &report.facts.tmux.set_clipboard,
            TmuxOptionFact::Available(value)
                if !spec.healthy_values.contains(&value.as_str())
        ),
        TmuxEvidence::DcsPassthrough => {
            report.facts.tmux.allow_passthrough_support == TmuxSupportFact::Supported
                && matches!(
                    &report.facts.tmux.allow_passthrough,
                    TmuxOptionFact::Available(value)
                        if !spec.healthy_values.contains(&value.as_str())
                )
        }
        TmuxEvidence::ExtendedKeys => matches!(
            &report.facts.tmux.extended_keys,
            TmuxOptionFact::Available(value) if value == "off"
        ),
    }
}

fn tmux_config_path(request: &FixRequest, terminal: &TerminalContext) -> Result<PathBuf, FixError> {
    if terminal.byobu != Some(ByobuBackend::Tmux) {
        return Ok(request.home.join(".tmux.conf"));
    }
    Ok(SafeAbsoluteDirectory::parse(
        request
            .byobu_config_dir
            .as_ref()
            .ok_or(FixError::ByobuConfigUnavailable)?
            .to_path_buf(),
        "BYOBU_CONFIG_DIR",
    )?
    .join(".tmux.conf"))
}

fn planned_change(managed: &ManagedConfigPlan) -> Result<PlannedChange, FixError> {
    planned_change_with_error(managed, FixError::PostconditionFailed)
}

fn planned_tmux_change(managed: &ManagedConfigPlan) -> Result<PlannedChange, FixError> {
    planned_change_with_error(managed, FixError::TmuxPostconditionFailed)
}

fn planned_change_with_error(
    managed: &ManagedConfigPlan,
    missing_block: FixError,
) -> Result<PlannedChange, FixError> {
    Ok(PlannedChange {
        requested_path: managed.requested_path().to_path_buf(),
        target_path: managed.target_path().to_path_buf(),
        block: managed.managed_block().ok_or(missing_block)?,
        backup_path_hint: managed.backup_path_hint().map(Path::to_path_buf),
        will_write: managed.changes_file(),
    })
}

pub fn apply_fix(plan: FixPlan) -> Result<FixOutcome, FixError> {
    let id = plan.id;
    match plan.payload {
        FixPayload::SshWrap(payload) => {
            let shell = payload.shell;
            let outcome = ManagedConfig::apply(payload.managed)?;
            if !managed_alias_configured(&outcome.target_path, shell) {
                return Err(FixError::PostconditionFailed);
            }
            Ok(fix_outcome(
                id,
                outcome,
                FixActivation::SatisfiedNow,
                Some(shell),
            ))
        }
        FixPayload::TmuxOption(payload) => {
            if payload.direct_state == DirectOptionState::Healthy {
                ManagedConfig::verify_unchanged(&payload.managed).map_err(FixError::TmuxManaged)?;
                let path = payload.managed.requested_path().to_path_buf();
                if !tmux_option_configured(&path, payload.spec) {
                    return Err(FixError::TmuxPostconditionFailed);
                }
                return Ok(FixOutcome::new(
                    id,
                    FixStatus::AlreadyConfigured,
                    ChangedFile {
                        path,
                        backup_path: None,
                    },
                    FixActivation::RequiresReload,
                    None,
                ));
            }
            let outcome = ManagedConfig::apply(payload.managed).map_err(FixError::TmuxManaged)?;
            if !tmux_option_configured(&outcome.target_path, payload.spec) {
                return Err(FixError::TmuxPostconditionFailed);
            }
            Ok(fix_outcome(
                id,
                outcome,
                FixActivation::RequiresReload,
                None,
            ))
        }
    }
}

fn fix_outcome(
    id: DiagnosticId,
    outcome: ManagedConfigOutcome,
    activation: FixActivation,
    shell: Option<ShellKind>,
) -> FixOutcome {
    FixOutcome::new(
        id,
        match outcome.status {
            ManagedConfigStatus::Applied => FixStatus::Applied,
            ManagedConfigStatus::NoChange => FixStatus::AlreadyConfigured,
        },
        ChangedFile {
            path: outcome.requested_path,
            backup_path: outcome.backup_path,
        },
        activation,
        shell,
    )
}

pub(crate) fn format_fix_success(outcome: &FixOutcome) -> String {
    let path = markdown_code_path(outcome.changed_path());
    let Some(kind) = fix_spec(outcome.id).map(|spec| spec.kind) else {
        return "Applied the Doctor fix.".to_owned();
    };
    let status = match (kind, outcome.status) {
        (FixKind::SshWrap, FixStatus::Applied) => format!("Set up SSH wrapping in {path}."),
        (FixKind::SshWrap, FixStatus::AlreadyConfigured) => {
            format!("SSH wrapping is already set up in {path}.")
        }
        (FixKind::TmuxOption(tmux), FixStatus::Applied) => {
            format!("Added `{}` to {path}.", tmux.line)
        }
        (FixKind::TmuxOption(tmux), FixStatus::AlreadyConfigured) => {
            format!("`{}` is already configured in {path}.", tmux.line)
        }
    };
    let backup = outcome
        .backup_path()
        .map(|path| format!("\nBackup: {}", path.display()))
        .unwrap_or_default();
    let activation = match (kind, outcome.activation) {
        (FixKind::SshWrap, FixActivation::SatisfiedNow) => {
            "\nStart a new shell to use the alias.".to_owned()
        }
        (FixKind::TmuxOption(tmux), FixActivation::RequiresReload) => format!(
            "\n{}\nRun /doctor again to verify the live setting.",
            tmux_activation_instruction(tmux, outcome.changed_path())
        ),
        _ => String::new(),
    };
    format!("{status}{backup}{activation}")
}

pub fn verify_persistent_fix(outcome: &FixOutcome) -> bool {
    let Some(spec) = fix_spec(outcome.id) else {
        return false;
    };
    match spec.kind {
        FixKind::SshWrap => false,
        FixKind::TmuxOption(tmux) => tmux_option_configured(outcome.changed_path(), tmux),
    }
}

fn preview_path(path: &Path) -> String {
    path.to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .map(commonmark_code_span)
        .unwrap_or_else(|| "[path cannot be rendered safely]".to_owned())
}

fn markdown_code_path(path: &Path) -> String {
    path.to_str()
        .map(commonmark_code_span)
        .unwrap_or_else(|| "the configured tmux file".to_owned())
}

fn commonmark_code_span(value: &str) -> String {
    let delimiter_len = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let delimiter = "`".repeat(delimiter_len);
    format!("{delimiter}{value}{delimiter}")
}

fn shell_quote_path(path: &Path) -> Option<String> {
    let value = path.to_str()?;
    if value
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return None;
    }
    Some(format!("'{}'", value.replace('\'', "'\\''")))
}

/// An accumulating remedy needs both steps: the server reads the new option
/// only on reload, and a client resolves its feature set only at attach, so
/// neither reloading nor reattaching alone changes anything.
fn tmux_activation_instruction(spec: &TmuxOptionSpec, path: &Path) -> String {
    match spec.remedy {
        TmuxRemedy::Assignment => reload_instruction(path),
        TmuxRemedy::Accumulating => match shell_quote_path(path) {
            Some(shell_path) => format!(
                "Run {}, then detach and reattach: only clients that attach after the reload get \
                 24-bit color.",
                commonmark_code_span(&format!("tmux source-file {shell_path}"))
            ),
            None => "Reload your tmux config, then detach and reattach: only clients that attach \
                     after the reload get 24-bit color."
                .to_owned(),
        },
    }
}

fn reload_instruction(path: &Path) -> String {
    let Some(shell_path) = shell_quote_path(path) else {
        return "Reload your tmux config, or restart the tmux server, to activate the persistent setting.".to_owned();
    };
    let command = format!("tmux source-file {shell_path}");
    format!(
        "Reload tmux with {}, or restart the tmux server.",
        commonmark_code_span(&command)
    )
}

pub fn managed_alias_configured(path: &Path, shell: ShellKind) -> bool {
    let request = ManagedConfigRequest {
        path: path.to_path_buf(),
        namespace: MANAGED_NAMESPACE.to_owned(),
        owned_item_prefix: "terminal.".to_owned(),
        items: vec![ManagedItem::new(SSH_WRAP_ID.to_string(), shell.alias())],
        comments: CommentSyntax::hash(),
        validator: None,
    };
    ManagedConfig::plan(request).is_ok_and(|plan| {
        !plan.changes_file()
            && detect_ssh_customization(plan.inspection().unmanaged_text(), shell).is_none()
    })
}

fn tmux_option_configured(path: &Path, spec: &'static TmuxOptionSpec) -> bool {
    let request = ManagedConfigRequest {
        path: path.to_path_buf(),
        namespace: MANAGED_NAMESPACE.to_owned(),
        owned_item_prefix: "terminal.".to_owned(),
        items: vec![ManagedItem::new(spec.id.to_string(), spec.line)],
        comments: CommentSyntax::hash(),
        validator: None,
    };
    ManagedConfig::plan(request).is_ok_and(|plan| match spec.remedy {
        TmuxRemedy::Accumulating => !plan.changes_file(),
        TmuxRemedy::Assignment => {
            let direct = scan_direct_tmux_option(
                plan.inspection().unmanaged_text(),
                plan.target_path(),
                spec,
            );
            matches!(direct, Ok(DirectOptionState::Healthy))
                || !plan.changes_file() && matches!(direct, Ok(DirectOptionState::Absent))
        }
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectOptionState {
    Absent,
    Healthy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TmuxOptionScope {
    Server,
    Window,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TmuxCommandToken<'a> {
    value: &'a str,
    quoted: bool,
}

fn scan_direct_tmux_option(
    text: &str,
    path: &Path,
    spec: &TmuxOptionSpec,
) -> Result<DirectOptionState, FixError> {
    let commands = tmux_top_level_commands(text, path, spec)?;
    let mut saw_healthy = false;
    for command in commands {
        let tokens = tokenize_tmux_command(&command, path, spec)?;
        if tokens.is_empty() {
            continue;
        }
        match classify_tmux_assignment(&tokens, spec) {
            TmuxAssignment::NotTarget => {}
            TmuxAssignment::Healthy => saw_healthy = true,
            TmuxAssignment::Conflict(detail) | TmuxAssignment::Ambiguous(detail) => {
                return Err(tmux_customization_error(path, spec, &detail));
            }
        }
    }
    Ok(if saw_healthy {
        DirectOptionState::Healthy
    } else {
        DirectOptionState::Absent
    })
}

fn tmux_top_level_commands(
    text: &str,
    path: &Path,
    spec: &TmuxOptionSpec,
) -> Result<Vec<String>, FixError> {
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut conditional_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut line_start = true;
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if escaped {
            if character == '\n' {
                // tmux removes escaped newlines exactly; it does not insert a space.
            } else {
                current.push(character);
            }
            escaped = false;
            line_start = character == '\n';
            index += 1;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            index += 1;
            continue;
        }
        if let Some(active_quote) = quote {
            current.push(character);
            if character == active_quote {
                quote = None;
            }
            line_start = character == '\n';
            index += 1;
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            current.push(character);
            line_start = false;
            index += 1;
            continue;
        }
        if character == '#'
            && (line_start || current.chars().last().is_some_and(char::is_whitespace))
        {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if line_start && character == '%' {
            let directive = chars[index..]
                .iter()
                .take_while(|character| **character != '\n')
                .collect::<String>();
            let directive = directive.trim();
            if directive.starts_with("%if") {
                conditional_depth = conditional_depth.saturating_add(1);
            } else if directive.starts_with("%endif") {
                conditional_depth = conditional_depth.saturating_sub(1);
            }
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            line_start = true;
            continue;
        }
        if character == '{' {
            brace_depth = brace_depth.saturating_add(1);
        } else if character == '}' {
            brace_depth = brace_depth.saturating_sub(1);
        }
        if matches!(character, ';' | '\n') {
            if conditional_depth == 0 && brace_depth == 0 && !current.trim().is_empty() {
                commands.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            line_start = true;
        } else {
            current.push(character);
            line_start = false;
        }
        index += 1;
    }
    if (escaped || quote.is_some() || conditional_depth != 0 || brace_depth != 0)
        && text.contains(spec.option)
    {
        return Err(tmux_customization_error(
            path,
            spec,
            "unterminated or ambiguous tmux syntax",
        ));
    }
    if conditional_depth == 0 && brace_depth == 0 && !current.trim().is_empty() {
        commands.push(current);
    }
    Ok(commands)
}

fn tokenize_tmux_command<'a>(
    command: &'a str,
    path: &Path,
    spec: &TmuxOptionSpec,
) -> Result<Vec<TmuxCommandToken<'a>>, FixError> {
    let bytes = command.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        while bytes[index..].first().is_some_and(u8::is_ascii_whitespace) {
            index += 1;
            if index == bytes.len() {
                return Ok(tokens);
            }
        }
        let start = index;
        let mut quote = None;
        let mut quoted = false;
        while index < bytes.len() {
            let byte = bytes[index];
            if let Some(active) = quote {
                if byte == active {
                    quote = None;
                }
                index += 1;
                continue;
            }
            if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
                quoted = true;
                index += 1;
                continue;
            }
            if byte.is_ascii_whitespace() {
                break;
            }
            index += 1;
        }
        if quote.is_some() {
            return Err(tmux_customization_error(
                path,
                spec,
                "unterminated quoted tmux token",
            ));
        }
        let raw = &command[start..index];
        let value = raw
            .strip_prefix(['\'', '"'])
            .and_then(|value| value.strip_suffix(['\'', '"']))
            .unwrap_or(raw);
        tokens.push(TmuxCommandToken { value, quoted });
    }
    Ok(tokens)
}

enum TmuxAssignment {
    NotTarget,
    Healthy,
    Conflict(String),
    Ambiguous(String),
}

fn classify_tmux_assignment(
    tokens: &[TmuxCommandToken<'_>],
    spec: &TmuxOptionSpec,
) -> TmuxAssignment {
    let mut index = 0;
    while tokens.get(index).is_some_and(|token| {
        !token.quoted && token.value.contains('=') && !token.value.starts_with('-')
    }) {
        index += 1;
    }
    let Some(command) = tokens.get(index) else {
        return TmuxAssignment::NotTarget;
    };
    if command.quoted {
        return TmuxAssignment::NotTarget;
    }
    let command_scope = match command.value {
        "set" | "set-option" | "seto" => None,
        "setw" | "set-window-option" => Some(TmuxOptionScope::Window),
        value
            if "set-option".starts_with(value)
                || "set".starts_with(value)
                || "set-window-option".starts_with(value) =>
        {
            if command_may_target(tokens, spec) {
                return TmuxAssignment::Ambiguous(format!(
                    "ambiguous tmux command prefix `{value}` may target `{}`",
                    spec.option
                ));
            }
            return TmuxAssignment::NotTarget;
        }
        _ => return TmuxAssignment::NotTarget,
    };
    index += 1;
    let mut explicit_scope = command_scope;
    let mut is_global = false;
    let mut has_target = false;
    while let Some(token) = tokens.get(index) {
        if token.quoted || !token.value.starts_with('-') || token.value == "-" {
            break;
        }
        if token.value == "--" {
            index += 1;
            break;
        }
        let flags = &token.value[1..];
        is_global |= flags.contains('g');
        if flags.contains('s') {
            explicit_scope = Some(TmuxOptionScope::Server);
        }
        if flags.contains('w') || flags.contains('p') {
            explicit_scope = Some(TmuxOptionScope::Window);
        }
        if flags.contains('t') {
            has_target = true;
            index += 1;
            if tokens.get(index).is_none() {
                return TmuxAssignment::Ambiguous("missing tmux target argument".to_owned());
            }
        }
        // -F, -f, -t and similar flags take one following argument. Unknown
        // flags on a possible target fail closed instead of shifting tokens.
        if flags.chars().any(|flag| matches!(flag, 'F' | 'f')) {
            index += 1;
            if tokens.get(index).is_none() {
                return TmuxAssignment::Ambiguous("missing tmux flag argument".to_owned());
            }
        }
        index += 1;
    }
    let Some(option) = tokens.get(index) else {
        return TmuxAssignment::NotTarget;
    };
    if option.quoted || option.value.starts_with('@') {
        return TmuxAssignment::NotTarget;
    }
    if option.value != spec.option {
        if spec.option.starts_with(option.value) {
            return TmuxAssignment::Ambiguous(format!(
                "option prefix `{}` may target `{}`",
                option.value, spec.option
            ));
        }
        return TmuxAssignment::NotTarget;
    }

    let effective_scope = explicit_scope.unwrap_or(spec.scope);
    match spec.scope {
        TmuxOptionScope::Server => {
            // tmux resolves known server options by option scope even when a
            // window flag is supplied. A target is nonsensical/ambiguous here.
            if has_target {
                return TmuxAssignment::Ambiguous(format!(
                    "targeted server assignment may affect `{}`",
                    spec.option
                ));
            }
        }
        TmuxOptionScope::Window => {
            // Only the global window value is persistent for future windows.
            // Local/targeted forms neither satisfy nor override that value.
            if effective_scope != TmuxOptionScope::Window || !is_global || has_target {
                return TmuxAssignment::NotTarget;
            }
        }
    }
    if tokens.len() != index + 2 || tokens[index + 1].quoted {
        return TmuxAssignment::Ambiguous(format!(
            "ambiguous direct assignment of `{}`",
            spec.option
        ));
    }
    let value = tokens[index + 1].value;
    if spec.healthy_values.contains(&value) {
        TmuxAssignment::Healthy
    } else {
        TmuxAssignment::Conflict(format!(
            "direct `{} {value}` conflicts with `{}`",
            spec.option, spec.line
        ))
    }
}

fn command_may_target(tokens: &[TmuxCommandToken<'_>], spec: &TmuxOptionSpec) -> bool {
    tokens.iter().skip(1).any(|token| {
        !token.quoted
            && !token.value.starts_with('@')
            && (token.value == spec.option || spec.option.starts_with(token.value))
    })
}

fn tmux_customization_error(path: &Path, spec: &TmuxOptionSpec, detail: &str) -> FixError {
    FixError::ExistingCustomization {
        path: path.to_path_buf(),
        detail: format!("{detail} for `{}`", spec.option),
    }
}

fn validator_for(_shell: ShellKind, override_path: Option<PathBuf>) -> Option<SyntaxValidator> {
    let program = override_path?;
    Some(SyntaxValidator {
        program,
        args: vec!["-n".into()],
        timeout: Duration::from_secs(2),
    })
}

fn resolve_validator_program(shell: &Path) -> Option<PathBuf> {
    let kind = ShellKind::from_shell_path(shell)?;
    if shell.components().count() > 1 {
        return executable_file(shell).then(|| shell.to_path_buf());
    }
    find_on_path(kind.name())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    find_on_path_in(name, std::env::split_paths(&std::env::var_os("PATH")?))
}

fn find_on_path_in<P>(name: &str, directories: impl IntoIterator<Item = P>) -> Option<PathBuf>
where
    P: AsRef<Path>,
{
    directories
        .into_iter()
        .map(|directory| directory.as_ref().join(name))
        .find(|candidate| executable_file(candidate))
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable_file(path: &Path) -> bool {
    path.is_file()
}

fn detect_ssh_customization(text: &str, shell: ShellKind) -> Option<String> {
    match shell {
        ShellKind::Bash | ShellKind::Zsh => detect_posix_ssh_customization(text),
        ShellKind::Fish => detect_fish_ssh_customization(text),
    }
}

fn detect_posix_ssh_customization(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if is_posix_ssh_alias_declaration(trimmed) {
            return Some("existing `alias ssh=...`".to_owned());
        }
        if is_posix_ssh_function_declaration(trimmed) {
            return Some("existing `ssh` shell function".to_owned());
        }
    }
    None
}

fn is_posix_ssh_alias_declaration(line: &str) -> bool {
    let Some(after_name) =
        after_shell_keyword(line, "alias").and_then(|rest| rest.strip_prefix("ssh"))
    else {
        return false;
    };
    after_name.starts_with('=')
        || (after_name.chars().next().is_some_and(char::is_whitespace)
            && after_name.trim_start().starts_with('='))
}

fn is_posix_ssh_function_declaration(line: &str) -> bool {
    let after_function = after_shell_keyword(line, "function");
    if after_function.is_some_and(|rest| token_is_exact_name(rest, "ssh")) {
        return true;
    }

    let Some(after_name) = line.strip_prefix("ssh") else {
        return false;
    };
    let after_name = after_name.trim_start();
    after_name.starts_with("()")
        || (after_name.starts_with('(') && after_name[1..].trim_start().starts_with(')'))
}

fn token_is_exact_name(text: &str, name: &str) -> bool {
    let Some(rest) = text.strip_prefix(name) else {
        return false;
    };
    rest.is_empty()
        || rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_whitespace() || ch == '(' || ch == '{')
}

fn after_shell_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(keyword)?;
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then_some(rest.trim_start())
}

fn detect_fish_ssh_customization(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if after_shell_keyword(trimmed, "alias").is_some_and(|rest| {
            token_is_exact_name(rest, "ssh")
                || rest
                    .strip_prefix("ssh")
                    .is_some_and(|rest| rest.starts_with('='))
        }) {
            return Some("existing `alias ssh ...`".to_owned());
        }
        if after_shell_keyword(trimmed, "function")
            .is_some_and(|rest| token_is_exact_name(rest, "ssh"))
        {
            return Some("existing `ssh` fish function".to_owned());
        }
    }
    None
}

fn actual_home() -> Option<PathBuf> {
    #[allow(deprecated)]
    std::env::home_dir()
}

pub fn configured_report(mut report: DiagnosticReport, configured: bool) -> DiagnosticReport {
    if configured {
        report.findings.retain(|finding| finding.id != SSH_WRAP_ID);
    }
    report
}

#[cfg(test)]
pub(crate) fn test_fix_plan(home: &Path) -> FixPlan {
    plan_fix(
        tests::request(home, "/bin/bash"),
        &tests::report(),
        &TerminalContext::default(),
    )
    .unwrap()
}

#[cfg(test)]
#[path = "fix_tests.rs"]
mod tests;
