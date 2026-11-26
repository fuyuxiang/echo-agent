//! Terminal detection utilities.
//!
//! Detects terminal emulator, multiplexer, and Byobu from environment variables.
//! Pure env-map helpers (`detect_*_from_env`) enable full matrix testing.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::host::HostOs;

pub mod da2;
pub mod embedded_editor;
pub mod hyperlinks;
pub mod image;
pub mod keyboard;
pub mod kitty_keyboard;
pub mod overlay;
pub(crate) mod probe;
pub mod term_version;
pub mod tmux;
pub mod tmux_probe;
pub mod xtversion;

pub use tmux::{passthrough_available, should_wrap_osc11, tmux_passthrough, tmux_passthrough_str};

pub use embedded_editor::{EmbeddedEditor, embedded_editor_from_env};
pub use hyperlinks::{
    HyperlinkCapabilities, Osc8Support, SchemeFilter, SetDefaultCursor, SetPointerCursor,
    hyperlink_capabilities,
};
pub use keyboard::{
    KeyboardCapabilities, ModifierDelivery, ModifierFate, keyboard_capabilities,
    keyboard_capabilities_for_host,
};
pub use kitty_keyboard::{
    kitty_event_types_withheld, kitty_flags_pushed, kitty_releases_reported,
    negotiated_kitty_flags, pushed_kitty_flags, set_pushed_kitty_flags, take_kitty_flags_pushed,
};
pub use term_version::{TermVersion, TermVersionSource};

#[cfg(test)]
mod test;

/// Test-only: build an env `HashMap` from pairs. Shared by the `test` submodule
/// and `embedded_editor`'s tests so the helper isn't duplicated.
#[cfg(test)]
pub(crate) fn env_from(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// Known terminal emulator categories.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, strum::Display)]
pub enum TerminalName {
    /// Apple Terminal (Terminal.app).
    #[strum(to_string = "Apple Terminal")]
    AppleTerminal,
    /// Ghostty terminal emulator.
    Ghostty,
    /// iTerm2 terminal emulator.
    #[strum(to_string = "iTerm2")]
    Iterm2,
    /// Warp terminal emulator.
    #[strum(to_string = "Warp")]
    WarpTerminal,
    /// VS Code integrated terminal.
    #[strum(to_string = "VS Code")]
    VsCode,
    /// Integrated terminal for the `Cursor` brand (VS Code family).
    Cursor,
    /// Integrated terminal for the `Windsurf` brand (VS Code family).
    Windsurf,
    /// Zed editor integrated terminal.
    Zed,
    /// WezTerm terminal emulator.
    WezTerm,
    /// kitty terminal emulator.
    #[strum(to_string = "Kitty")]
    Kitty,
    /// Alacritty terminal emulator.
    Alacritty,
    /// Rio terminal emulator.
    Rio,
    /// foot terminal emulator (Wayland-native, Linux-only). Full native
    /// Kitty keyboard protocol support. Detected via TERM.
    #[strum(to_string = "foot")]
    Foot,
    /// JetBrains IDE integrated terminal (JediTerm — IntelliJ, PhpStorm, etc.).
    /// No runtime capability probing is possible (no TERM_FEATURES, no
    /// XTVERSION, DA1 is bare VT102). Classic vs Reworked 2025 engine is
    /// indistinguishable. All capabilities are conservative/Unknown.
    #[strum(to_string = "JetBrains")]
    JetBrains,
    /// EchoAgent Desktop (Electron app).
    #[strum(to_string = "EchoAgent Desktop")]
    EchoAgentDesktop,
    /// VTE-based terminal (GNOME Terminal, kgx/GNOME Console, Tilix, etc.).
    #[strum(to_string = "VTE")]
    Vte,
    /// Terminator terminal emulator (Python/GTK, VTE-based). Detected via the
    /// `TERMINATOR_UUID` env var it exports on every child process, or
    /// `TERM_PROGRAM=terminator`.
    Terminator,
    /// Windows Terminal (wt, the default terminal on Windows 11+).
    #[strum(to_string = "Windows Terminal")]
    WindowsTerminal,
    /// Otty (otty.sh). Wraps macOS IME commits in bracketed paste.
    #[strum(to_string = "Otty")]
    Otty,
    /// Unknown terminal.
    #[default]
    Unknown,
}

impl TerminalName {
    pub fn is_vte_based(self) -> bool {
        matches!(self, Self::Vte | Self::Terminator) // WHY: single source of truth for the VTE family
    }

    /// VS Code integrated terminal and xterm.js-based IDE embeds (including forks).
    pub fn is_vscode_family(self) -> bool {
        matches!(
            self,
            Self::VsCode | Self::Cursor | Self::Windsurf | Self::Zed
        )
    }

    /// Terminals that embed xterm.js (Zed's terminal is alacritty-based and
    /// is NOT one). The boundary for xterm.js-specific quirks, e.g. the
    /// wedged button tracker that eats mouse releases.
    pub fn is_xtermjs_embed(self) -> bool {
        matches!(
            self,
            Self::VsCode | Self::Cursor | Self::Windsurf | Self::EchoAgentDesktop
        )
    }

    /// Brands whose capabilities are not positively classified — share
    /// [`Self::Unknown`]'s fail-closed posture (no KKP probe, conservative
    /// hyperlinks/notifications/focus, etc.).
    pub fn is_capability_unclassified(self) -> bool {
        matches!(self, Self::Unknown | Self::Otty)
    }

    /// Host applies OSC 52 writes to the system pasteboard (fail closed).
    pub fn supports_osc52_clipboard(self) -> bool {
        matches!(
            self,
            Self::Ghostty
                | Self::Kitty
                | Self::WezTerm
                | Self::Alacritty
                | Self::Foot
                | Self::Rio
                | Self::WindowsTerminal
                | Self::Iterm2
                | Self::VsCode
                | Self::Cursor
                | Self::Windsurf
                | Self::Zed
        )
    }

    /// Only Otty is known to wrap macOS IME commits in bracketed paste.
    pub fn delivers_ime_as_bracketed_paste(self) -> bool {
        matches!(self, Self::Otty)
    }
}

impl TerminalContext {
    pub fn is_vte_based(&self) -> bool {
        self.brand.is_vte_based() || self.vte_version.is_some() // WHY: covers brand + legacy version marker
    }
}

/// The kind of terminal multiplexer wrapping the session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, strum::Display)]
pub enum MultiplexerKind {
    /// tmux (including Byobu-on-tmux).
    #[strum(to_string = "tmux")]
    Tmux,
    /// GNU screen (including Byobu-on-screen).
    #[strum(to_string = "GNU screen")]
    Screen,
    /// Zellij.
    Zellij,
    /// cmux (Ghostty-backed macOS terminal multiplexer).
    #[strum(to_string = "cmux")]
    Cmux,
    /// herdr, a libghostty-backed agent multiplexer
    /// ([ogulcancelik/herdr](https://github.com/ogulcancelik/herdr)).
    ///
    /// Its embedded emulator answers CSI queries itself, so it counts as
    /// CSI-intercepting. The accepted cost: a herdr pane typically has no
    /// other version signal (brand `Unknown`, no `TERM_PROGRAM_VERSION`), but
    /// the XTVERSION reply describes herdr's engine rather than the host.
    #[strum(to_string = "herdr")]
    Herdr,
    /// No recognized multiplexer detected (does not rule out unknown ones).
    #[default]
    #[strum(to_string = "None detected")]
    Undetected,
}

impl MultiplexerKind {
    /// Whether this multiplexer intercepts CSI queries (e.g. XTVERSION)
    /// instead of passing them through to the outer terminal. See
    /// [`Self::Herdr`] for the version signal herdr gives up by being here.
    pub fn intercepts_csi_queries(self) -> bool {
        matches!(self, Self::Tmux | Self::Screen | Self::Zellij | Self::Herdr)
    }
}

/// The Byobu backend, when Byobu markers are present.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, strum::Display)]
pub enum ByobuBackend {
    /// Unknown backend (default; never produced by detection).
    #[default]
    Unknown,
    /// Byobu wrapping tmux.
    #[strum(to_string = "tmux")]
    Tmux,
    /// Byobu wrapping GNU screen.
    #[strum(to_string = "GNU screen")]
    Screen,
}

/// Cached tmux client metadata needed for downstream policy decisions.
///
/// Fields are gathered at startup from environment variables; no live
/// subprocess calls are made here. Live tmux-option queries remain in
/// [`crate::diagnostics`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TmuxClientMeta {
    /// The raw `TMUX` variable value (e.g. `/tmp/tmux-501/default,12345,0`).
    pub tmux_env: Option<String>,
    /// The `TMUX_PANE` value (e.g. `%0`).
    pub tmux_pane: Option<String>,
}

/// Full terminal context serving as the single source of
/// truth that later features (warnings, fullscreen policy,
/// clipboard routing) should consume.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TerminalContext {
    /// The effective terminal emulator brand for capability and display
    /// decisions. On native Windows an `Unknown` env detection is resolved
    /// to [`TerminalName::WindowsTerminal`] (see
    /// `refine_unknown_brand_for_host`).
    pub brand: TerminalName,
    /// The raw brand from environment detection, before the native-Windows
    /// `Unknown -> WindowsTerminal` fallback applied to `brand`. Consult this
    /// (not `brand`) for conservative, default-deny decisions that must not
    /// trust the assumed brand — e.g. the legacy-Windows glyph fallback and
    /// the unidentified-terminal `Shift+Enter` gate.
    pub env_brand: TerminalName,
    /// The detected multiplexer wrapping the session.
    pub multiplexer: MultiplexerKind,
    /// Whether Byobu is wrapping the session, and which backend it uses.
    pub byobu: Option<ByobuBackend>,
    /// Which embedded editor `:terminal` echoagent is running inside, if any.
    pub embedded_editor: Option<EmbeddedEditor>,
    /// tmux client metadata (populated only when `multiplexer == Tmux`).
    pub tmux_meta: TmuxClientMeta,
    /// Whether the session is inside a remote SSH connection.
    pub is_ssh: bool,
    /// Positive evidence that SSH is hosted by the official VS Code remote server.
    pub is_official_vscode_remote: bool,
    /// The raw `TERM` environment variable (e.g. `xterm-256color`, `screen`).
    pub term_var: Option<String>,
    /// The tmux server version (e.g. `"tmux 3.4"`), populated only when
    /// `multiplexer == Tmux`. Detected via `tmux -V` subprocess at startup.
    pub tmux_version: Option<String>,
    /// The VTE version (e.g. `"7402"` for VTE 0.74.2).
    /// `None` when not running inside a VTE terminal.
    pub vte_version: Option<String>,
    /// Value of tmux's `extended-keys` global option (`"on"`, `"off"`,
    /// `"always"`); populated only when `multiplexer == Tmux`.
    pub tmux_extended_keys: Option<String>,
    /// The `TERM_PROGRAM_VERSION` environment variable, falling back to
    /// `LC_TERMINAL_VERSION` (e.g. `"3.5.6"` for iTerm2, `"1.1.3"` for
    /// Ghostty). Used for version-gating features that require a minimum
    /// terminal version. Raw and **ungated**: inside tmux this is tmux's own
    /// version. For a brand-corroborated value use [`Self::env_term_version`].
    pub term_program_version: Option<String>,
    /// The brand-corroborated counterpart to `term_program_version` (see
    /// [`term_version`]). Resolved once in [`build_terminal_context_from_env`],
    /// so it does not re-derive if `brand` or `vte_version` change afterwards.
    pub env_term_version: Option<TermVersion>,
}

impl TerminalContext {
    /// Returns `true` if the session is inside any tmux-backed environment
    /// (plain tmux or Byobu-on-tmux).
    pub fn is_tmux_backed(&self) -> bool {
        self.multiplexer == MultiplexerKind::Tmux
    }

    /// Per-terminal keyboard capabilities for this brand on the current host.
    pub fn keyboard_capabilities(&self) -> KeyboardCapabilities {
        keyboard_capabilities(self.brand)
    }

    /// Per-terminal hyperlink (OSC 8) capabilities for this brand.
    pub fn hyperlink_capabilities(&self) -> HyperlinkCapabilities {
        hyperlink_capabilities(self.brand)
    }

    /// Returns `true` if the session is inside Byobu regardless of backend.
    pub fn is_byobu(&self) -> bool {
        self.byobu.is_some()
    }

    /// Whether an outer layer (embedded-editor :terminal or multiplexer) can
    /// repaint our pane out of band, stranding rows until a full clear. A heal
    /// keyed off this only fires when a FocusGained actually reaches echoagent, which
    /// needs focus reporting enabled upstream (e.g. tmux `focus-events on`, off
    /// by default).
    pub fn repaints_pane_out_of_band(&self) -> bool {
        self.embedded_editor.is_some() || self.multiplexer != MultiplexerKind::Undetected
    }

    /// Returns the tmux config path appropriate for the environment.
    ///
    /// In Byobu-on-tmux, this is `~/.byobu/.tmux.conf`; otherwise
    /// `~/.tmux.conf`.
    pub fn tmux_config_path(&self) -> String {
        if self.byobu == Some(ByobuBackend::Tmux) {
            "~/.byobu/.tmux.conf".to_owned()
        } else {
            "~/.tmux.conf".to_owned()
        }
    }

    /// Returns the `TERM` variable value, or `"n/a"` if unset.
    pub fn term_var_or_na(&self) -> &str {
        self.term_var.as_deref().unwrap_or("n/a")
    }

    /// Returns the tmux version string, or `"n/a"` if not in tmux or
    /// detection failed.
    pub fn tmux_version_or_na(&self) -> &str {
        self.tmux_version.as_deref().unwrap_or("n/a")
    }

    /// Returns the reason to skip Kitty keyboard flags, or `None` if the
    /// environment is compatible.
    ///
    /// Terminal-emulator reasons (vscode, apple_terminal, vte, windows_terminal) take
    /// precedence over multiplexer reasons so the user is pointed at the
    /// deeper cause.
    pub fn kitty_skip_reason(&self) -> Option<&'static str> {
        let is_tmux_3_3_later = self.is_tmux_version_or_later(3, 3);
        if matches!(
            self.brand,
            TerminalName::VsCode
                | TerminalName::Cursor
                | TerminalName::Windsurf
                | TerminalName::Zed
        ) {
            return Some("vscode");
        }
        if self.brand == TerminalName::AppleTerminal {
            return Some("apple_terminal");
        }
        if self.is_vte_based() {
            // WHY: central helper replaces VTE duplication
            return Some("vte");
        }
        if self.brand == TerminalName::WindowsTerminal {
            return Some("windows_terminal");
        }
        if self.brand == TerminalName::JetBrains {
            return Some("jetbrains");
        }
        if self.multiplexer == MultiplexerKind::Screen {
            return Some("screen");
        }
        if self.multiplexer == MultiplexerKind::Tmux && !is_tmux_3_3_later {
            return Some("tmux_old");
        }
        if self.multiplexer == MultiplexerKind::Tmux
            && is_tmux_3_3_later
            && self.tmux_extended_keys.as_deref() == Some("off")
        {
            return Some("tmux_extended_keys_off");
        }
        // No positive evidence of KKP support — skip to avoid xterm.js
        // mis-encoding shifted keys (https://github.com/xtermjs/xterm.js/issues/5823).
        // Probing an unresponsive terminal blocks startup.
        if self.brand.is_capability_unclassified()
            && self.multiplexer == MultiplexerKind::Undetected
        {
            return Some("unknown_no_multiplexer");
        }
        None
    }

    /// Returns the reason inline images / terminal graphics protocols are
    /// disabled, or `None` if the environment is compatible.
    pub fn graphics_protocol_skip_reason(&self) -> Option<&'static str> {
        if self.is_tmux_backed() {
            return Some("tmux");
        }
        None
    }

    /// Whether this terminal leaks mouse-tracking reports into the input as raw
    /// text instead of consuming them, corrupting the prompt.
    ///
    /// JediTerm (JetBrains IDEs) on Windows is the known offender: crossterm's
    /// Windows input source only decodes native console mouse records, not the
    /// VT `\e[M…` byte stream JediTerm emits, so those bytes surface as key
    /// presses. macOS/Linux crossterm parses them, so the leak is Windows-only.
    /// The pager defaults these sessions to minimal mode (no mouse capture).
    pub fn mouse_reporting_leaks_as_raw_text(&self) -> bool {
        mouse_reporting_leaks(self.brand, HostOs::current())
    }

    /// Whether the running terminal cannot distinguish `Shift+Enter` from
    /// bare `Enter` at the byte level, so the UI should advertise
    /// `Alt+Enter` for newline insertion instead.
    ///
    /// Distinguishing `Shift+Enter` requires the Kitty keyboard protocol
    /// (KKP) to be negotiated. This returns `true` for the environments
    /// where the pager cannot rely on KKP for a usable `Shift+Enter`:
    ///
    /// 1. **Legacy VTE** (GNOME Terminal, Ptyxis, kgx, Tilix, etc.) whose
    ///    `VTE_VERSION` is below `8200` (VTE 0.82.0, the first release with
    ///    KKP, merged in
    ///    [MR !14](https://gitlab.gnome.org/GNOME/vte/-/merge_requests/14)).
    ///    Also true when the brand is detected as VTE but `VTE_VERSION` is
    ///    missing or unparseable — we conservatively assume old.
    /// 2. **VS Code's integrated terminal (xterm.js) and VS Code-family /
    ///    xterm.js IDE forks**. xterm.js only partially implements KKP —
    ///    it mis-encodes shifted printable keys — so the pager deliberately
    ///    never negotiates KKP for them (see [`Self::kitty_skip_reason`]
    ///    `== "vscode"` and [xterm.js#5823](https://github.com/xtermjs/xterm.js/issues/5823)).
    ///    Without KKP, xterm.js sends a bare `CR` for `Shift+Enter`,
    ///    byte-for-byte identical to `Enter`.
    /// 3. **Unidentified terminals with no multiplexer**, where the pager
    ///    also skips KKP (no positive evidence of support — typically
    ///    VS Code's xterm.js reached over SSH, where `TERM_PROGRAM` isn't
    ///    forwarded and the brand falls back to `Unknown`).
    ///
    /// In every case `Alt+Enter` (delivered as `ESC`+`CR`) is the reliable
    /// newline chord and is what the UI advertises.
    pub fn shift_enter_unavailable(&self) -> bool {
        let is_vte = self.is_vte_based(); // WHY: central helper + version gating
        if is_vte {
            return match self
                .vte_version
                .as_deref()
                .and_then(|v| v.parse::<u32>().ok())
            {
                Some(ver) => ver < 8200,
                // Brand=Vte but no parseable version — conservative: assume old.
                None => true,
            };
        }

        // VS Code / xterm.js and its forks: KKP is never negotiated, so
        // Shift+Enter arrives as a bare CR indistinguishable from Enter.
        if matches!(
            self.brand,
            TerminalName::VsCode
                | TerminalName::Cursor
                | TerminalName::Windsurf
                | TerminalName::Zed
        ) {
            return true;
        }

        // Unidentified / unclassified brand with no multiplexer: KKP is
        // skipped (see `kitty_skip_reason`). This is the common
        // VS Code-over-SSH shape (brand falls back to Unknown). On native
        // Windows the effective `brand` is refined to WindowsTerminal, so
        // consult `env_brand` — a bare ConHost is still env-Unknown and
        // must advertise Alt+Enter even though we optimistically treat it
        // as WT for capabilities.
        if self.env_brand.is_capability_unclassified()
            && self.multiplexer == MultiplexerKind::Undetected
        {
            return true;
        }

        false
    }

    /// True when `Ctrl+.` cannot be delivered reliably as a shortcuts primary.
    ///
    /// Without KKP (or an equivalent extended-key path), `Ctrl+.` is not a
    /// classic C0 control and collapses to `.` / an ambiguous byte — so we
    /// follow [`Self::kitty_skip_reason`] rather than a hard-coded brand
    /// list. That keeps iTerm2+tmux with `extended-keys off` (and other
    /// multiplexer skips) aligned with VS Code / VTE / Apple Terminal.
    /// The pager folds in host-OS signals (Windows, WSL) via
    /// `ctrl_dot_unreliable()`.
    pub fn ctrl_dot_unreliable(&self) -> bool {
        self.kitty_skip_reason().is_some()
    }

    /// Returns the reason to skip hyperlink (OSC 8) emission, or `None`
    /// if the environment is compatible.
    ///
    /// Terminal-emulator reasons take precedence over multiplexer reasons
    /// so the user is pointed at the deeper cause.
    pub fn hyperlink_skip_reason(&self) -> Option<&'static str> {
        let caps = self.hyperlink_capabilities();
        let is_tmux_3_4_later = self.is_tmux_version_or_later(3, 4);
        if caps.osc8 == Osc8Support::HostileParser {
            return Some("apple_terminal");
        }
        if caps.osc8 == Osc8Support::Unsupported {
            return Some("unsupported_terminal");
        }
        // VTE < 0.50.4 (version int < 5004) does not handle OSC 8 cleanly.
        // Checked before multiplexer so a VTE+old-tmux user is pointed at the
        // VTE upgrade rather than chasing tmux config.
        if let Some(ref vte_ver) = self.vte_version
            && let Ok(ver_int) = vte_ver.parse::<u32>()
            && ver_int < 5004
        {
            return Some("vte_old");
        }
        if caps.osc8 == Osc8Support::Unknown {
            return Some("unknown_terminal");
        }
        if self.multiplexer == MultiplexerKind::Screen {
            return Some("screen");
        }
        if self.multiplexer == MultiplexerKind::Tmux && !is_tmux_3_4_later {
            return Some("tmux_old");
        }
        None
    }

    /// Returns `true` if the detected tmux version is `major`.`minor` or later.
    pub fn is_tmux_version_or_later(&self, major: u32, minor: u32) -> bool {
        match self
            .tmux_version
            .as_deref()
            .and_then(parse_tmux_major_minor)
        {
            Some((maj, min)) => (maj, min) >= (major, minor),
            // Unknown version — conservative: assume old.
            None => false,
        }
    }

    /// Returns `true` if `TERM_PROGRAM_VERSION` is `major`.`minor` or later.
    /// Returns `false` when the env var is absent or unparseable.
    pub fn is_term_program_version_or_later(&self, major: u32, minor: u32) -> bool {
        match self
            .term_program_version
            .as_deref()
            .and_then(parse_semver_major_minor)
        {
            Some((maj, min)) => (maj, min) >= (major, minor),
            None => false,
        }
    }

    /// The best available terminal version and the source that reported it.
    ///
    /// Not pure: the DA2 arm reads process-global probe state, so env-precedence
    /// tests hold only while no reply has been recorded in the process.
    pub fn term_version(&self) -> (String, TermVersionSource) {
        term_version::best_term_version(da2::detected(), self.env_term_version.as_ref())
    }

    /// Extract a flat snapshot of terminal details for telemetry.
    pub fn telemetry_snapshot(&self) -> echo_agent_telemetry::events::TerminalTelemetry {
        let os = crate::host::HostOs::current();
        let server = crate::host::DisplayServer::current();
        let kb = self.keyboard_capabilities();
        let route = crate::clipboard::clipboard_route();
        let (term_version, term_version_source) = self.term_version();
        echo_agent_telemetry::events::TerminalTelemetry {
            brand: self.brand.to_string(),
            multiplexer: self.multiplexer.to_string(),
            is_ssh: self.is_ssh,
            is_byobu: self.is_byobu(),
            term_var: self.term_var_or_na().to_owned(),
            host_os: os.to_string(),
            display_server: server.to_string(),
            modifier_cmd_fate: kb.modifier_delivery.cmd.to_string(),
            modifier_opt_fate: kb.modifier_delivery.opt.to_string(),
            enter_modifier_fate: kb.enter_modifier.to_string(),
            tmux_version: self.tmux_version_or_na().to_owned(),
            xtversion: xtversion::detected().unwrap_or("").to_owned(),
            term_version,
            term_version_source: term_version_source.to_string(),
            kitty_event_types_withheld: kitty_event_types_withheld(),
            hyperlink_osc8: self.hyperlink_capabilities().osc8.to_string(),
            hyperlink_skip_reason: self.hyperlink_skip_reason().unwrap_or("none").to_owned(),
            clipboard_route: route.to_string(),
            clipboard_native_tool: echo_agent_shared::clipboard::native_tool_name().to_owned(),
            clipboard_data_control: crate::clipboard::wayland_data_control_label().to_owned(),
        }
    }

    /// Extract terminal info for feedback submissions.
    pub fn feedback_info(&self) -> echo_agent_shared::session::FeedbackTerminalInfo {
        use echo_agent_shared::session::FeedbackTerminalInfo;
        // XTVERSION self-report lets feedback triage identify the terminal
        // even when env detection failed (e.g. over SSH).
        let brand = match xtversion::detected() {
            Some(v) if self.brand == TerminalName::Unknown => format!("Unknown (XTVERSION: {v})"),
            _ => self.brand.to_string(),
        };
        // Raw and unlabeled by design: no provenance, and no rewrite of DA2's
        // library version into an Alacritty release number.
        let (term_version, _source) = self.term_version();
        FeedbackTerminalInfo {
            brand,
            multiplexer: self.multiplexer.to_string(),
            is_ssh: self.is_ssh,
            is_byobu: self.is_byobu(),
            term_var: self.term_var_or_na().to_owned(),
            term_version: (!term_version.is_empty()).then_some(term_version),
            tmux_version: if self.is_tmux_backed() {
                self.tmux_version.clone()
            } else {
                None
            },
            hyperlink_osc8_support: Some(self.hyperlink_capabilities().osc8.to_string()),
            clipboard_route: Some(crate::clipboard::clipboard_route().to_string()),
            clipboard_native_tool: Some(
                echo_agent_shared::clipboard::native_tool_name().to_owned(),
            ),
            display_server: Some(crate::host::DisplayServer::current().to_string()),
        }
    }
}

static TERMINAL_CONTEXT: OnceLock<TerminalContext> = OnceLock::new();

/// Returns the cached terminal context for the current process.
///
/// This is the preferred entry point for new code that needs multiplexer
/// or Byobu information. The context is computed once at first access from
/// process environment variables.
pub fn terminal_context() -> &'static TerminalContext {
    TERMINAL_CONTEXT.get_or_init(detect_terminal_context)
}

/// Detect terminal environment facts without any live tmux subprocesses.
///
/// Standalone diagnostics use this so an unhealthy tmux server cannot block
/// before the diagnostic runner has a chance to report unavailable evidence.
pub fn standalone_terminal_context() -> TerminalContext {
    standalone_terminal_context_from_env(&collect_process_env(), HostOs::current())
}

fn standalone_terminal_context_from_env(
    env: &HashMap<String, String>,
    host: HostOs,
) -> TerminalContext {
    let mut ctx = build_terminal_context_from_env(env);
    ctx.brand = refine_unknown_brand_for_host(ctx.brand, host);
    ctx
}

/// Build a [`TerminalContext`] from the current process environment.
fn detect_terminal_context() -> TerminalContext {
    let env = collect_process_env();
    // NOTE: brand is usually Unknown in tmux (overwrites TERM_PROGRAM,
    // per-pane vars don't survive) and over SSH (not forwarded), except
    // brands with SSH-surviving markers (the VS Code family, and iTerm2
    // via LC_TERMINAL). tmux -g global env is stale (reflects the server's
    // first client, not the current one). Revisit when `echoagent ssh` can
    // forward env vars.
    let mut ctx = build_terminal_context_from_env(&env);
    ctx.brand = refine_unknown_brand_for_host(ctx.brand, HostOs::current());
    if ctx.is_tmux_backed() {
        ctx.tmux_version = detect_tmux_version();
        ctx.tmux_extended_keys = detect_tmux_extended_keys();
    }
    ctx
}

/// Collect the process environment into a `HashMap` for the pure helpers.
fn collect_process_env() -> HashMap<String, String> {
    crate::host::collect_unicode_env()
}

/// Helper to look up a key in the env map, returning `Some` for non-empty
/// values.
fn env_get<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(|v| v.as_str()).filter(|v| !v.is_empty())
}

fn is_official_vscode_remote_askpass(path: &str) -> bool {
    std::path::Path::new(path).components().any(|component| {
        matches!(
            component,
            std::path::Component::Normal(name)
                if name == ".vscode-server" || name == ".vscode-server-insiders"
        )
    })
}

/// Detect the terminal brand from an injected environment map.
///
/// This is the pure equivalent of the original `detect_terminal_info`.
///
/// Adding a new env marker to this brand chain (or to
/// [`detect_byobu_from_env`] / [`detect_multiplexer_from_env`] below)
/// requires extending `HOST_TERMINAL_ENV_VARS` in
/// `echo-agent-pager-pty-harness/src/pty.rs` (test-env hygiene — the PTY
/// harness strips every marker read here so the host terminal can't leak
/// into tests).
pub fn detect_terminal_brand_from_env(env: &HashMap<String, String>) -> TerminalName {
    // Some VS Code forks set TERM_PROGRAM=vscode, so check IDE-specific
    // env vars first to disambiguate them from upstream VS Code.
    //
    // These markers also survive cases where TERM_PROGRAM does not: plain
    // SSH (TERM_PROGRAM not forwarded) and tmux (TERM_PROGRAM overwritten
    // by the multiplexer). Without them, brand falls back to Unknown and
    // clipboard/keyboard gates that key off VS Code family miss the session.
    if env_get(env, "CURSOR_TRACE_ID").is_some() {
        return TerminalName::Cursor;
    }
    if let Some(askpass) = env_get(env, "VSCODE_GIT_ASKPASS_MAIN") {
        let askpass_lower = askpass.to_ascii_lowercase();
        if askpass_lower.contains("cursor") {
            return TerminalName::Cursor;
        }
        if askpass_lower.contains("windsurf") {
            return TerminalName::Windsurf;
        }
        // Pure VS Code remote agent injects this even without TERM_PROGRAM.
        return TerminalName::VsCode;
    }

    // TERM_PROGRAM is the most reliable signal.
    if let Some(term_program) = env_get(env, "TERM_PROGRAM")
        && let Some(name) = terminal_name_from_term_program(term_program)
    {
        return name;
    }

    // JetBrains IDE terminal (JediTerm). All JetBrains IDEs (IntelliJ,
    // PhpStorm, WebStorm, etc.) set TERMINAL_EMULATOR=JetBrains-JediTerm.
    // Both the Classic and Reworked 2025 engine set the same value — there
    // is no env var to distinguish them, no TERM_FEATURES equivalent, and
    // XTVERSION queries leak as garbage (DA1 returns bare VT102 `?6c`).
    // We're effectively blind to capabilities; conservative defaults only.
    //
    // Must be checked before TERM_SESSION_ID: JetBrains sets that too
    // (cross-platform, including Windows), which otherwise false-positives
    // as Apple Terminal.
    if let Some(te) = env_get(env, "TERMINAL_EMULATOR") {
        let te_lower = te.to_ascii_lowercase();
        if te_lower.contains("jetbrains") || te_lower.contains("jediterm") {
            return TerminalName::JetBrains;
        }
    }

    // WezTerm-specific variable.
    if env_get(env, "WEZTERM_VERSION").is_some() {
        return TerminalName::WezTerm;
    }

    // iTerm2. LC_TERMINAL=iTerm2 survives SSH (SendEnv/AcceptEnv LC_*)
    // where ITERM_SESSION_ID / TERM_PROGRAM do not.
    if env_get(env, "ITERM_SESSION_ID").is_some()
        || env_get(env, "ITERM_PROFILE").is_some()
        || env_get(env, "LC_TERMINAL").is_some_and(|v| v.eq_ignore_ascii_case("iterm2"))
    {
        return TerminalName::Iterm2;
    }

    // Apple Terminal.
    if env_get(env, "TERM_SESSION_ID").is_some() {
        return TerminalName::AppleTerminal;
    }

    // Kitty.
    if env_get(env, "KITTY_WINDOW_ID").is_some() {
        return TerminalName::Kitty;
    }
    if let Some(term) = env_get(env, "TERM")
        && term.contains("kitty")
    {
        return TerminalName::Kitty;
    }

    // Alacritty.
    if env_get(env, "ALACRITTY_SOCKET").is_some() {
        return TerminalName::Alacritty;
    }
    if let Some(term) = env_get(env, "TERM")
        && term == "alacritty"
    {
        return TerminalName::Alacritty;
    }

    // Rio.
    if let Some(term) = env_get(env, "TERM")
        && term == "rio"
    {
        return TerminalName::Rio;
    }

    // foot (Wayland-native, Linux-only). foot sets no unique env var, only
    // TERM. Match its exact terminfo names to avoid over-matching.
    if let Some(term) = env_get(env, "TERM")
        && matches!(term, "foot" | "foot-extra" | "foot-direct")
    {
        return TerminalName::Foot;
    }

    // Terminator (Python/GTK, VTE-based) exports TERMINATOR_UUID on every
    // child process. Check before the generic VTE_VERSION fallback so it is
    // identified specifically rather than as a generic VTE terminal (it sets
    // both).
    if env_get(env, "TERMINATOR_UUID").is_some() {
        return TerminalName::Terminator;
    }

    // VTE-based terminals (GNOME Terminal, kgx, Tilix, etc.).
    if env_get(env, "VTE_VERSION").is_some() {
        return TerminalName::Vte;
    }

    // Windows Terminal sets WT_SESSION (a GUID) on every child process.
    if env_get(env, "WT_SESSION").is_some() {
        return TerminalName::WindowsTerminal;
    }

    TerminalName::Unknown
}

/// Resolve an `Unknown` brand to `WindowsTerminal` on native Windows.
///
/// Windows Terminal is the Windows 11 default, but its DefTerm handoff
/// starts the first shell without WT_SESSION/TERM_PROGRAM
/// (microsoft/terminal#13006), so env detection misses it. The raw
/// detection stays in [`TerminalContext::env_brand`] for consumers that must
/// not trust this guess. WSL is unaffected (its Linux binary reports
/// `HostOs::Linux`).
fn refine_unknown_brand_for_host(brand: TerminalName, host: HostOs) -> TerminalName {
    if brand == TerminalName::Unknown && host == HostOs::Windows {
        TerminalName::WindowsTerminal
    } else {
        brand
    }
}

/// Core predicate for [`TerminalContext::mouse_reporting_leaks_as_raw_text`],
/// split out so the brand/host matrix is unit-testable without the real host.
fn mouse_reporting_leaks(brand: TerminalName, host: HostOs) -> bool {
    brand == TerminalName::JetBrains && host == HostOs::Windows
}

/// Detect the Byobu wrapper state from an injected environment map.
///
/// Returns `Some(ByobuBackend)` when Byobu markers are present and the
/// backend can be determined. Byobu sets `BYOBU_BACKEND` to `"tmux"` or
/// `"screen"`. When `BYOBU_BACKEND` is absent but other Byobu markers
/// exist (`BYOBU_CONFIG_DIR`, `BYOBU_DISTRO`), we infer the backend from
/// the multiplexer markers `TMUX` and `STY`.
pub fn detect_byobu_from_env(env: &HashMap<String, String>) -> Option<ByobuBackend> {
    let has_byobu_backend = env_get(env, "BYOBU_BACKEND").is_some();
    let has_byobu_config = env_get(env, "BYOBU_CONFIG_DIR").is_some();
    let has_byobu_distro = env_get(env, "BYOBU_DISTRO").is_some();

    if !has_byobu_backend && !has_byobu_config && !has_byobu_distro {
        return None;
    }

    // Explicit backend marker takes precedence.
    if let Some(backend) = env_get(env, "BYOBU_BACKEND") {
        return match backend.to_ascii_lowercase().as_str() {
            "tmux" => Some(ByobuBackend::Tmux),
            "screen" => Some(ByobuBackend::Screen),
            // Unknown backend string — fall through to inference.
            _ => infer_byobu_backend_from_mux_markers(env),
        };
    }

    // No explicit BYOBU_BACKEND but other markers present — infer.
    infer_byobu_backend_from_mux_markers(env)
}

/// When Byobu markers exist but `BYOBU_BACKEND` is absent or unrecognised,
/// infer the backend from which multiplexer markers are set.
fn infer_byobu_backend_from_mux_markers(env: &HashMap<String, String>) -> Option<ByobuBackend> {
    let has_tmux = env_get(env, "TMUX").is_some();
    let has_sty = env_get(env, "STY").is_some();

    match (has_tmux, has_sty) {
        (true, _) => Some(ByobuBackend::Tmux),
        (false, true) => Some(ByobuBackend::Screen),
        // Byobu markers with neither TMUX nor STY — cannot determine.
        (false, false) => None,
    }
}

/// Detect the multiplexer kind from an injected environment map.
///
/// Precedence rules for ambiguous marker combinations:
/// 1. Explicit `BYOBU_BACKEND` beats generic `TMUX`/`STY` clues.
/// 2. `TMUX` beats `ZELLIJ` (tmux can nest inside Zellij but not vice-versa).
/// 3. `STY` (GNU screen) is only chosen when neither `TMUX` nor `ZELLIJ` is set.
/// 4. herdr and cmux markers classify only when no tmux/zellij/screen (or
///    explicit Byobu backend) won — a real mux nested inside either still
///    wins. Between the two, `HERDR_ENV` beats the `CMUX_*` markers: the
///    shape they appear in together is herdr running in a cmux panel, where
///    the `CMUX_*` values are inherited rather than fresh.
///
/// This ensures one deterministic classification even when multiple markers
/// are present (e.g., an inherited `ZELLIJ` var inside a tmux pane).
pub fn detect_multiplexer_from_env(env: &HashMap<String, String>) -> MultiplexerKind {
    let byobu = detect_byobu_from_env(env);

    // If Byobu is detected with an explicit backend, let that win.
    if let Some(backend) = byobu {
        match backend {
            ByobuBackend::Tmux => return MultiplexerKind::Tmux,
            ByobuBackend::Screen => return MultiplexerKind::Screen,
            ByobuBackend::Unknown => {} // fall through to standard markers
        }
    }

    // Standard multiplexer markers, tmux > Zellij > screen.
    // Nested real multiplexers inside herdr/cmux must win over the host mux.
    // A herdr daemon first started from tmux freezes that TMUX into every pane:
    // classified tmux here, so OSC 52 gets a tmux DCS wrap herdr renders as text.
    if env_get(env, "TMUX").is_some() {
        return MultiplexerKind::Tmux;
    }
    if env_get(env, "ZELLIJ").is_some() || env_get(env, "ZELLIJ_SESSION_NAME").is_some() {
        return MultiplexerKind::Zellij;
    }
    if env_get(env, "STY").is_some() {
        return MultiplexerKind::Screen;
    }
    // herdr sets HERDR_ENV=1 in every pane, overrides TERM to xterm-256color
    // and never sets TERM_PROGRAM, so this marker is its only documented,
    // stable signal.
    if env_get(env, "HERDR_ENV").is_some() {
        return MultiplexerKind::Herdr;
    }
    // cmux sets non-empty CMUX_SOCKET_PATH / CMUX_PANEL_ID / CMUX_BUNDLE_ID;
    // CMUX_SOCKET may be present but empty — env_get filters empties.
    if env_get(env, "CMUX_SOCKET_PATH").is_some()
        || env_get(env, "CMUX_PANEL_ID").is_some()
        || env_get(env, "CMUX_BUNDLE_ID").is_some()
    {
        return MultiplexerKind::Cmux;
    }

    MultiplexerKind::Undetected
}

/// Extract tmux client metadata from an injected environment map.
pub fn detect_tmux_meta_from_env(env: &HashMap<String, String>) -> TmuxClientMeta {
    TmuxClientMeta {
        tmux_env: env_get(env, "TMUX").map(|s| s.to_owned()),
        tmux_pane: env_get(env, "TMUX_PANE").map(|s| s.to_owned()),
    }
}

/// Build a full [`TerminalContext`] from an injected environment map.
///
/// This is the primary pure helper — call it with a controlled env map in
/// tests to exercise the full detection matrix without ambient host state.
pub fn build_terminal_context_from_env(env: &HashMap<String, String>) -> TerminalContext {
    let brand = detect_terminal_brand_from_env(env);
    let multiplexer = detect_multiplexer_from_env(env);
    let byobu = detect_byobu_from_env(env);
    let embedded_editor = embedded_editor_from_env(env);
    let tmux_meta = if multiplexer == MultiplexerKind::Tmux {
        detect_tmux_meta_from_env(env)
    } else {
        TmuxClientMeta::default()
    };
    let is_ssh = env_get(env, "SSH_CONNECTION").is_some()
        || env_get(env, "SSH_TTY").is_some()
        || env_get(env, "SSH_CLIENT").is_some();
    let is_official_vscode_remote = is_ssh
        && env_get(env, "VSCODE_GIT_ASKPASS_MAIN").is_some_and(is_official_vscode_remote_askpass);
    let term_var = env_get(env, "TERM").map(|s| s.to_owned());
    let vte_version = env_get(env, "VTE_VERSION").map(|s| s.to_owned());
    // SSH strips TERM_PROGRAM_VERSION; iTerm2 LC_TERMINAL_VERSION survives.
    let term_program_version = env_get(env, "TERM_PROGRAM_VERSION")
        .or_else(|| env_get(env, "LC_TERMINAL_VERSION"))
        .map(|s| s.to_owned());
    // Resolved here: the brand each version var must corroborate is in hand.
    let env_term_version = term_version::detect_env_term_version(env, brand);

    TerminalContext {
        brand,
        env_brand: brand,
        multiplexer,
        byobu,
        embedded_editor,
        tmux_meta,
        is_ssh,
        is_official_vscode_remote,
        term_var,
        tmux_version: None,
        vte_version,
        tmux_extended_keys: None,
        term_program_version,
        env_term_version,
    }
}

/// Map TERM_PROGRAM value to terminal name.
fn terminal_name_from_term_program(value: &str) -> Option<TerminalName> {
    let normalized: String = value
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_' | '.'))
        .map(|c| c.to_ascii_lowercase())
        .collect();

    match normalized.as_str() {
        "appleterminal" => Some(TerminalName::AppleTerminal),
        "ghostty" => Some(TerminalName::Ghostty),
        "iterm" | "iterm2" | "itermapp" => Some(TerminalName::Iterm2),
        "warp" | "warpterminal" => Some(TerminalName::WarpTerminal),
        "vscode" => Some(TerminalName::VsCode),
        "wezterm" => Some(TerminalName::WezTerm),
        "kitty" => Some(TerminalName::Kitty),
        "alacritty" => Some(TerminalName::Alacritty),
        "rio" => Some(TerminalName::Rio),
        "terminator" => Some(TerminalName::Terminator),
        "zed" => Some(TerminalName::Zed),
        "echoagentdesktop" => Some(TerminalName::EchoAgentDesktop),
        "windowsterminal" => Some(TerminalName::WindowsTerminal),
        "otty" => Some(TerminalName::Otty),
        _ => None,
    }
}

/// User-configured alt-screen (fullscreen) mode.
///
/// Parsed from `[terminal] alt_screen` in `~/.echo-agent/pager.toml` and
/// overridden by the `--no-alt-screen` CLI flag.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AltScreenMode {
    /// Automatic: fullscreen in plain terminals and normal tmux, inline in
    /// tmux control mode and Zellij.
    #[default]
    Auto,
    /// Always enter the alternate screen, even in environments where auto
    /// would disable it (e.g. tmux control mode, Zellij).
    Always,
    /// Never enter the alternate screen — always run inline.
    Never,
}

/// Detect whether the current tmux session is in control mode.
///
/// Returns `false` when not inside tmux or when the query fails.
pub fn detect_tmux_control_mode(ctx: &TerminalContext) -> bool {
    if ctx.multiplexer != MultiplexerKind::Tmux {
        return false;
    }
    tmux_probe::query_control_mode()
        .into_option()
        .unwrap_or(false)
}

/// Detect the tmux server version by running `tmux -V`.
pub fn detect_tmux_version() -> Option<String> {
    tmux_probe::query_version().into_option()
}

/// Trimmed value of tmux's global `extended-keys` option.
pub fn detect_tmux_extended_keys() -> Option<String> {
    tmux_probe::query_option("extended-keys").into_option()
}

/// Parse major.minor from a plain semver-ish string like `"3.6.0"` or `"1.2"`.
fn parse_semver_major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Parse the major.minor version from a tmux version string like `"tmux 3.4"`
/// or `"tmux 3.3a"`. Returns `None` on unrecognized formats.
fn parse_tmux_major_minor(version: &str) -> Option<(u32, u32)> {
    let rest = version.strip_prefix("tmux ")?;
    let mut parts = rest.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor_str = parts.next()?;
    let minor_end = minor_str
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(minor_str.len());
    let minor: u32 = minor_str[..minor_end].parse().ok()?;
    Some((major, minor))
}

/// Resolve the effective alt-screen (fullscreen) state from CLI override,
/// config, and environment.
///
/// Precedence:
/// 1. `--no-alt-screen` CLI flag → always inline
/// 2. `config_mode` from `[terminal] alt_screen` → Always/Never/Auto
/// 3. Auto rules:
///    - Zellij → inline
///    - tmux control mode → inline
///    - otherwise → fullscreen
///
/// Returns `true` when the pager should enter the alternate screen.
pub fn determine_alt_screen_policy(
    cli_no_alt_screen: bool,
    config_mode: AltScreenMode,
    ctx: &TerminalContext,
    is_control_mode: bool,
) -> bool {
    // CLI override has highest precedence.
    if cli_no_alt_screen {
        return false;
    }

    match config_mode {
        AltScreenMode::Always => true,
        AltScreenMode::Never => false,
        AltScreenMode::Auto => {
            // Auto-disable in Zellij.
            if ctx.multiplexer == MultiplexerKind::Zellij {
                return false;
            }
            // Auto-disable in tmux control mode.
            if ctx.is_tmux_backed() && is_control_mode {
                return false;
            }
            // Default: fullscreen.
            true
        }
    }
}
