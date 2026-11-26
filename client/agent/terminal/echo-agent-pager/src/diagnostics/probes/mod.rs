//! Shared terminal observations for diagnostics consumers.

use crate::terminal::TerminalContext;

mod tmux;

pub use tmux::{LiveTmuxProbe, TmuxOptionQuery, TmuxProbeResult};

#[derive(Clone, Copy)]
pub struct TuiProbeEvidence<'a> {
    pub fullscreen_active: bool,
    pub kitty_flags_pushed: bool,
    pub xtversion: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEvidence<T> {
    Available(T),
    Unavailable,
}

#[derive(Clone, Copy)]
pub struct DiagnosticRuntimeEvidence<'a> {
    pub fullscreen_active: RuntimeEvidence<bool>,
    pub kitty_flags_pushed: RuntimeEvidence<bool>,
    pub xtversion: RuntimeEvidence<Option<&'a str>>,
}

impl<'a> From<TuiProbeEvidence<'a>> for DiagnosticRuntimeEvidence<'a> {
    fn from(value: TuiProbeEvidence<'a>) -> Self {
        Self {
            fullscreen_active: RuntimeEvidence::Available(value.fullscreen_active),
            kitty_flags_pushed: RuntimeEvidence::Available(value.kitty_flags_pushed),
            xtversion: RuntimeEvidence::Available(value.xtversion),
        }
    }
}

pub struct ProbeSnapshot<'a> {
    pub terminal: &'a TerminalContext,
    pub tmux: TmuxProbeFacts,
    pub wayland: WaylandProbeFacts,
    pub runtime: TuiProbeEvidence<'a>,
}

pub struct CommonProbeSnapshot<'a> {
    pub terminal: &'a TerminalContext,
    pub tmux: TmuxProbeFacts,
    pub wayland: WaylandProbeFacts,
}

pub struct StandaloneDiagnosticSnapshot<'a> {
    pub common: CommonProbeSnapshot<'a>,
    pub clipboard: ClipboardProbeFacts,
    pub host_os: crate::host::HostOs,
    pub display_server: crate::host::DisplayServer,
    pub container_no_display: bool,
    pub color_level: RuntimeEvidence<crate::theme::color_support::ColorLevel>,
}

pub struct DoctorProbeSnapshot<'a> {
    pub common: ProbeSnapshot<'a>,
    pub clipboard: ClipboardProbeFacts,
    pub host_os: crate::host::HostOs,
    pub display_server: crate::host::DisplayServer,
    pub container_no_display: bool,
    pub color_level: crate::theme::color_support::ColorLevel,
}

/// Whether the caller will read the colour-passthrough fact. Only `view()`
/// reads it, and the startup path never calls `view()`, so probing there
/// spends ~116ms before first paint on a value that is dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorPassthroughProbe {
    Skip,
    Run,
}

pub struct TmuxProbeFacts {
    pub version: TmuxProbeResult<String>,
    pub extended_keys: TmuxProbeResult<String>,
    pub set_clipboard: TmuxProbeResult<String>,
    pub allow_passthrough_support: TmuxProbeResult<()>,
    pub allow_passthrough: TmuxProbeResult<String>,
    pub control_mode: TmuxProbeResult<bool>,
    /// Comma-separated `client_termfeatures` for the attached client.
    pub client_features: TmuxProbeResult<String>,
}

#[derive(Clone)]
pub struct ClipboardProbeFacts {
    pub route: crate::clipboard::ClipboardRoute,
    pub native_tool: &'static str,
    pub osc52_sink_active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaylandProbeFacts {
    pub is_wayland: bool,
    pub data_control: TmuxProbeResult<bool>,
    pub wl_copy_available: bool,
}

pub fn osc52_sink_active() -> bool {
    crate::clipboard::osc52_sink_active()
}

pub fn collect_startup_tui<'a>(
    terminal: &'a TerminalContext,
    runtime: TuiProbeEvidence<'a>,
    control_mode: bool,
    tmux: &dyn TmuxOptionQuery,
) -> ProbeSnapshot<'a> {
    let is_wayland = crate::host::DisplayServer::current() == crate::host::DisplayServer::Wayland;
    let native_tool = startup_native_tool(is_wayland, || {
        echo_agent_runtime::util::clipboard::native_tool_name()
    });
    collect_common(
        terminal,
        runtime,
        Some(control_mode),
        ColorPassthroughProbe::Skip,
        tmux,
        is_wayland,
        native_tool,
    )
}

pub fn collect_doctor_tui<'a>(
    terminal: &'a TerminalContext,
    runtime: TuiProbeEvidence<'a>,
    tmux: &dyn TmuxOptionQuery,
) -> DoctorProbeSnapshot<'a> {
    let is_wayland = crate::host::DisplayServer::current() == crate::host::DisplayServer::Wayland;
    let native_tool = echo_agent_runtime::util::clipboard::native_tool_name();
    DoctorProbeSnapshot {
        common: collect_common(
            terminal,
            runtime,
            None,
            ColorPassthroughProbe::Run,
            tmux,
            is_wayland,
            Some(native_tool),
        ),
        clipboard: ClipboardProbeFacts {
            route: crate::clipboard::clipboard_route().clone(),
            native_tool,
            osc52_sink_active: osc52_sink_active(),
        },
        host_os: crate::host::HostOs::current(),
        display_server: crate::host::DisplayServer::current(),
        container_no_display: echo_agent_runtime::util::clipboard::is_containerized_without_display(
        ),
        color_level: crate::theme::color_support::get(),
    }
}

/// Collect standalone evidence without running live tmux subprocesses; skipped
/// tmux evidence is reported unavailable so a stuck server cannot block doctor.
pub fn collect_standalone<'a>(terminal: &'a TerminalContext) -> StandaloneDiagnosticSnapshot<'a> {
    collect_standalone_with_tmux(terminal, unavailable_tmux())
}

/// Collect bounded live tmux facts for explicit fix planning.
pub fn collect_standalone_fix<'a>(
    terminal: &'a TerminalContext,
    id: Option<crate::diagnostics::DiagnosticId>,
) -> StandaloneDiagnosticSnapshot<'a> {
    collect_standalone_with_tmux(terminal, collect_tmux_fix(terminal, id, &LiveTmuxProbe))
}

fn collect_tmux_fix(
    terminal: &TerminalContext,
    id: Option<crate::diagnostics::DiagnosticId>,
    tmux: &dyn TmuxOptionQuery,
) -> TmuxProbeFacts {
    if !terminal.is_tmux_backed() {
        return unavailable_tmux();
    }
    let wants = |candidate| id.is_none() || id == Some(candidate);
    let set_clipboard = if wants(crate::diagnostics::TMUX_CLIPBOARD_ID) {
        tmux.show_option("set-clipboard")
    } else {
        TmuxProbeResult::Unavailable
    };
    let extended_keys = if wants(crate::diagnostics::TMUX_EXTENDED_KEYS_ID) {
        tmux.show_option("extended-keys")
    } else {
        TmuxProbeResult::Unavailable
    };
    let (allow_passthrough_support, allow_passthrough) =
        if wants(crate::diagnostics::DCS_PASSTHROUGH_ID) {
            let support = tmux.option_support("allow-passthrough");
            let value = match &support {
                TmuxProbeResult::Available(()) => tmux.show_option("allow-passthrough"),
                TmuxProbeResult::Unsupported => TmuxProbeResult::Unsupported,
                TmuxProbeResult::Unavailable => TmuxProbeResult::Unavailable,
                TmuxProbeResult::Error(error) => TmuxProbeResult::Error(error.clone()),
            };
            (support, value)
        } else {
            (TmuxProbeResult::Unavailable, TmuxProbeResult::Unavailable)
        };
    let client_features = if wants(crate::diagnostics::TMUX_TRUECOLOR_ID) {
        tmux.client_features()
    } else {
        TmuxProbeResult::Unavailable
    };
    TmuxProbeFacts {
        version: TmuxProbeResult::Unavailable,
        extended_keys,
        set_clipboard,
        allow_passthrough_support,
        allow_passthrough,
        control_mode: TmuxProbeResult::Unavailable,
        client_features,
    }
}

fn collect_standalone_with_tmux<'a>(
    terminal: &'a TerminalContext,
    tmux: TmuxProbeFacts,
) -> StandaloneDiagnosticSnapshot<'a> {
    let host_os = crate::host::HostOs::current();
    let display_server = crate::host::DisplayServer::current();
    let is_wayland = display_server == crate::host::DisplayServer::Wayland;
    let native_tool = echo_agent_runtime::util::clipboard::native_tool_name();
    let data_control = standalone_data_control(is_wayland);
    let container_no_display =
        echo_agent_runtime::util::clipboard::is_containerized_without_display();
    collect_standalone_from(
        terminal,
        tmux,
        WaylandProbeFacts {
            is_wayland,
            data_control,
            wl_copy_available: is_wayland && native_tool == "wl-copy",
        },
        native_tool,
        crate::clipboard::resolve_clipboard_route(terminal),
        osc52_sink_active(),
        host_os,
        display_server,
        container_no_display,
        match crate::theme::color_support::standalone(terminal.brand) {
            crate::theme::color_support::StandaloneColorEvidence::Available(level) => {
                RuntimeEvidence::Available(level)
            }
            crate::theme::color_support::StandaloneColorEvidence::Unavailable => {
                RuntimeEvidence::Unavailable
            }
        },
    )
}

// Plumbing constructor: one argument per probe fact feeding the snapshot
// (crate precedent for probe/render assemblers, e.g. agent_view/render.rs).
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_standalone_from<'a>(
    terminal: &'a TerminalContext,
    tmux: TmuxProbeFacts,
    wayland: WaylandProbeFacts,
    native_tool: &'static str,
    route: crate::clipboard::ClipboardRoute,
    osc52_sink_active: bool,
    host_os: crate::host::HostOs,
    display_server: crate::host::DisplayServer,
    container_no_display: bool,
    color_level: RuntimeEvidence<crate::theme::color_support::ColorLevel>,
) -> StandaloneDiagnosticSnapshot<'a> {
    StandaloneDiagnosticSnapshot {
        common: CommonProbeSnapshot {
            terminal,
            tmux,
            wayland,
        },
        clipboard: ClipboardProbeFacts {
            route,
            native_tool,
            osc52_sink_active,
        },
        host_os,
        display_server,
        container_no_display,
        color_level,
    }
}

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

fn standalone_data_control(is_wayland: bool) -> TmuxProbeResult<bool> {
    if !is_wayland {
        return TmuxProbeResult::Unavailable;
    }
    match echo_agent_runtime::util::clipboard::probe_wayland_data_control() {
        echo_agent_runtime::util::clipboard::WaylandDataControlProbe::Available(value) => {
            TmuxProbeResult::Available(value)
        }
        echo_agent_runtime::util::clipboard::WaylandDataControlProbe::Unavailable => {
            TmuxProbeResult::Unavailable
        }
        echo_agent_runtime::util::clipboard::WaylandDataControlProbe::Error(error) => {
            TmuxProbeResult::Error(error)
        }
    }
}

// Plumbing constructor: one argument per probe input feeding the snapshot.
#[allow(clippy::too_many_arguments)]
fn collect_common<'a>(
    terminal: &'a TerminalContext,
    runtime: TuiProbeEvidence<'a>,
    control_mode: Option<bool>,
    color_probe: ColorPassthroughProbe,
    tmux: &dyn TmuxOptionQuery,
    is_wayland: bool,
    native_tool: Option<&str>,
) -> ProbeSnapshot<'a> {
    let data_control =
        is_wayland && echo_agent_runtime::util::clipboard::wayland_data_control_supported();
    ProbeSnapshot {
        terminal,
        tmux: collect_tmux(terminal, control_mode, color_probe, tmux),
        wayland: WaylandProbeFacts {
            is_wayland,
            data_control: TmuxProbeResult::Available(data_control),
            wl_copy_available: is_wayland && native_tool == Some("wl-copy"),
        },
        runtime,
    }
}

fn startup_native_tool(
    is_wayland: bool,
    native_tool: impl FnOnce() -> &'static str,
) -> Option<&'static str> {
    is_wayland.then(native_tool)
}

fn collect_tmux(
    terminal: &TerminalContext,
    control_mode: Option<bool>,
    color_probe: ColorPassthroughProbe,
    tmux: &dyn TmuxOptionQuery,
) -> TmuxProbeFacts {
    if !terminal.is_tmux_backed() {
        return unavailable_tmux();
    }

    let allow_passthrough_support = tmux.option_support("allow-passthrough");
    let allow_passthrough = match &allow_passthrough_support {
        TmuxProbeResult::Available(()) => tmux.show_option("allow-passthrough"),
        TmuxProbeResult::Unsupported => TmuxProbeResult::Unsupported,
        TmuxProbeResult::Unavailable => TmuxProbeResult::Unavailable,
        TmuxProbeResult::Error(error) => TmuxProbeResult::Error(error.clone()),
    };
    TmuxProbeFacts {
        version: terminal
            .tmux_version
            .clone()
            .map(TmuxProbeResult::Available)
            .unwrap_or(TmuxProbeResult::Unavailable),
        extended_keys: terminal
            .tmux_extended_keys
            .clone()
            .map(TmuxProbeResult::Available)
            .unwrap_or_else(|| tmux.show_option("extended-keys")),
        set_clipboard: tmux.show_option("set-clipboard"),
        allow_passthrough_support,
        allow_passthrough,
        control_mode: control_mode
            .map(TmuxProbeResult::Available)
            .unwrap_or_else(|| tmux.control_mode()),
        client_features: match color_probe {
            ColorPassthroughProbe::Run => tmux.client_features(),
            ColorPassthroughProbe::Skip => TmuxProbeResult::Unavailable,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use super::*;

    struct FakeTmuxQuery {
        values: HashMap<&'static str, TmuxProbeResult<String>>,
        control_mode: TmuxProbeResult<bool>,
        calls: RefCell<Vec<String>>,
    }

    impl TmuxOptionQuery for FakeTmuxQuery {
        fn show_option(&self, option: &str) -> TmuxProbeResult<String> {
            self.calls.borrow_mut().push(option.to_owned());
            self.values
                .get(option)
                .cloned()
                .unwrap_or(TmuxProbeResult::Unavailable)
        }

        fn option_support(&self, option: &str) -> TmuxProbeResult<()> {
            self.calls.borrow_mut().push(format!("support:{option}"));
            match self.values.get(option) {
                Some(TmuxProbeResult::Unsupported) => TmuxProbeResult::Unsupported,
                Some(TmuxProbeResult::Unavailable) | None => TmuxProbeResult::Unavailable,
                Some(TmuxProbeResult::Error(error)) => TmuxProbeResult::Error(error.clone()),
                Some(TmuxProbeResult::Available(_)) => TmuxProbeResult::Available(()),
            }
        }

        fn control_mode(&self) -> TmuxProbeResult<bool> {
            self.calls.borrow_mut().push("control-mode".to_owned());
            self.control_mode.clone()
        }

        fn client_features(&self) -> TmuxProbeResult<String> {
            self.calls.borrow_mut().push("client-features".to_owned());
            self.values
                .get("client-features")
                .cloned()
                .unwrap_or(TmuxProbeResult::Unavailable)
        }
    }

    fn runtime() -> TuiProbeEvidence<'static> {
        TuiProbeEvidence {
            fullscreen_active: true,
            kitty_flags_pushed: true,
            xtversion: Some("WezTerm 20240203"),
        }
    }

    fn empty_fake() -> FakeTmuxQuery {
        FakeTmuxQuery {
            values: HashMap::new(),
            control_mode: TmuxProbeResult::Unavailable,
            calls: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn supplied_control_mode_is_the_only_snapshot_fact() {
        let terminal = TerminalContext {
            multiplexer: crate::terminal::MultiplexerKind::Tmux,
            ..Default::default()
        };
        let fake = FakeTmuxQuery {
            values: HashMap::from([
                (
                    "set-clipboard",
                    TmuxProbeResult::Available("external".to_owned()),
                ),
                ("allow-passthrough", TmuxProbeResult::Unsupported),
            ]),
            control_mode: TmuxProbeResult::Available(false),
            calls: RefCell::new(Vec::new()),
        };

        let snapshot = collect_common(
            &terminal,
            runtime(),
            Some(true),
            ColorPassthroughProbe::Skip,
            &fake,
            false,
            None,
        );

        assert_eq!(snapshot.tmux.control_mode, TmuxProbeResult::Available(true));
        assert_eq!(
            snapshot.tmux.set_clipboard,
            TmuxProbeResult::Available("external".to_owned())
        );
        assert_eq!(
            snapshot.tmux.allow_passthrough_support,
            TmuxProbeResult::Unsupported
        );
        assert_eq!(
            fake.calls.into_inner(),
            [
                "support:allow-passthrough",
                "extended-keys",
                "set-clipboard"
            ]
        );
    }

    #[test]
    fn missing_control_mode_uses_backend() {
        let terminal = TerminalContext {
            multiplexer: crate::terminal::MultiplexerKind::Tmux,
            ..Default::default()
        };
        let fake = FakeTmuxQuery {
            control_mode: TmuxProbeResult::Available(true),
            ..empty_fake()
        };
        let snapshot = collect_common(
            &terminal,
            runtime(),
            None,
            ColorPassthroughProbe::Run,
            &fake,
            false,
            None,
        );

        assert_eq!(snapshot.tmux.control_mode, TmuxProbeResult::Available(true));
        assert_eq!(
            fake.calls.into_inner(),
            [
                "support:allow-passthrough",
                "extended-keys",
                "set-clipboard",
                "control-mode",
                "client-features",
            ]
        );
    }

    #[test]
    fn non_tmux_snapshot_skips_tmux_backend() {
        let terminal = TerminalContext::default();
        let fake = FakeTmuxQuery {
            control_mode: TmuxProbeResult::Error("must not run".to_owned()),
            ..empty_fake()
        };
        let snapshot = collect_common(
            &terminal,
            runtime(),
            None,
            ColorPassthroughProbe::Run,
            &fake,
            false,
            None,
        );

        assert_eq!(snapshot.tmux.set_clipboard, TmuxProbeResult::Unavailable);
        assert_eq!(snapshot.tmux.control_mode, TmuxProbeResult::Unavailable);
        assert!(fake.calls.into_inner().is_empty());
    }

    #[test]
    fn non_wayland_startup_does_not_resolve_native_tool() {
        let called = Cell::new(false);
        let native_tool = startup_native_tool(false, || {
            called.set(true);
            "fake-tool"
        });
        assert!(!called.get());
        assert_eq!(native_tool, None);
    }

    #[test]
    fn wayland_startup_resolves_native_tool_for_warning() {
        let called = Cell::new(false);
        let native_tool = startup_native_tool(true, || {
            called.set(true);
            "fake-tool"
        });
        assert!(called.get());
        assert_eq!(native_tool, Some("fake-tool"));
    }
}
