//! Welcome Sandbox | Local picker under `--chat`. CLI/env stamp wins at startup.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// Welcome-screen workspace selection (in-memory until session start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WelcomeWorkspaceMode {
    /// Backend sandbox / product-chat default.
    #[default]
    Sandbox,
    /// Local Computer Hub workspace server (own mode; replaces sandbox).
    LocalWorkspace,
}

impl WelcomeWorkspaceMode {
    /// Modes shown on the welcome picker under `--chat`.
    pub const ALL: [Self; 2] = [Self::Sandbox, Self::LocalWorkspace];

    pub fn cycle_next(self) -> Self {
        match self {
            Self::Sandbox => Self::LocalWorkspace,
            Self::LocalWorkspace => Self::Sandbox,
        }
    }

    pub fn cycle_prev(self) -> Self {
        self.cycle_next()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sandbox => "Sandbox",
            Self::LocalWorkspace => "Local workspace",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Sandbox => "backend sandbox",
            Self::LocalWorkspace => "this machine · Computer Hub",
        }
    }

    /// Compact in-session / status-bar label.
    pub fn status_label(self, cli_locked: bool) -> &'static str {
        match (self, cli_locked) {
            (Self::Sandbox, _) => "Sandbox",
            (Self::LocalWorkspace, true) => "Local·CLI",
            (Self::LocalWorkspace, false) => "Local",
        }
    }

    /// Unified-list `kind`: Sandbox → `chat`, Local → `build`.
    pub fn history_kind_filter(self) -> &'static str {
        match self {
            Self::Sandbox => "chat",
            Self::LocalWorkspace => "build",
        }
    }

    /// Conversation/gateway → Sandbox; other sources → Local.
    pub fn from_history_source(source: &str) -> Self {
        if source == "conversation" {
            Self::Sandbox
        } else {
            Self::LocalWorkspace
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Sandbox => 0,
            Self::LocalWorkspace => 1,
        }
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL[i % Self::ALL.len()]
    }
}

/// Structured log target for welcome / in-session workspace mode events.
pub const WORKSPACE_MODE_LOG: &str = "echoagent.pager.workspace_mode";

/// Log a welcome picker selection change (Ctrl+E cycle or click).
pub fn log_welcome_mode_selected(
    mode: WelcomeWorkspaceMode,
    via: &'static str,
    startup_locked: bool,
) {
    tracing::info!(
        target: WORKSPACE_MODE_LOG,
        event = "welcome_mode_selected",
        mode = mode.label(),
        history_kind = mode.history_kind_filter(),
        via,
        startup_locked,
        "welcome workspace mode selected"
    );
}

/// Log Local ACK confirm or cancel.
pub fn log_welcome_ack(outcome: &'static str) {
    tracing::info!(
        target: WORKSPACE_MODE_LOG,
        event = "welcome_local_ack",
        outcome,
        "welcome local-workspace ACK"
    );
}

/// Log one-shot / process stamp application for a new welcome session.
pub fn log_welcome_intent_applied(
    mode: WelcomeWorkspaceMode,
    startup_locked: bool,
    one_shot: &'static str,
    process_stamp: &'static str,
) {
    tracing::info!(
        target: WORKSPACE_MODE_LOG,
        event = "welcome_intent_applied",
        mode = mode.label(),
        startup_locked,
        one_shot,
        process_stamp,
        "welcome workspace intent applied for NewSession"
    );
}

/// Log CLI/env lock applied at startup (before any welcome selection).
pub fn log_cli_lock_applied(mode: WelcomeWorkspaceMode) {
    tracing::info!(
        target: WORKSPACE_MODE_LOG,
        event = "cli_lock_applied",
        mode = mode.label(),
        "CLI/env local-workspace lock applied at startup"
    );
}

/// Log CLI/env lock winning over a differing welcome selection.
pub fn log_cli_lock_wins(mode: WelcomeWorkspaceMode) {
    tracing::info!(
        target: WORKSPACE_MODE_LOG,
        event = "cli_lock_wins",
        mode = mode.label(),
        "CLI/env local-workspace lock wins; welcome selection ignored"
    );
}

/// In-session indicator: history bypass / local intent → Local; else Sandbox.
pub fn indicator_for_opening_session(
    chat_kind: bool,
    history_load_as_build: bool,
    cli_locked: bool,
    local_workspace_intent: bool,
) -> (WelcomeWorkspaceMode, bool) {
    if history_load_as_build {
        return (WelcomeWorkspaceMode::LocalWorkspace, cli_locked);
    }
    if chat_kind && !local_workspace_intent {
        return (WelcomeWorkspaceMode::Sandbox, false);
    }
    if cli_locked {
        return (WelcomeWorkspaceMode::LocalWorkspace, true);
    }
    if local_workspace_intent {
        return (WelcomeWorkspaceMode::LocalWorkspace, false);
    }
    (WelcomeWorkspaceMode::Sandbox, false)
}

/// Log session-list kind filter / history-source switch.
pub fn log_history_source(
    event: &'static str,
    mode: Option<WelcomeWorkspaceMode>,
    kind_filter: Option<&[String]>,
    source: Option<&str>,
) {
    tracing::info!(
        target: WORKSPACE_MODE_LOG,
        event,
        mode = mode.map(WelcomeWorkspaceMode::label),
        kind_filter = ?kind_filter,
        history_source = source,
        "workspace history source"
    );
}

/// Hit-test rects for each segmented option (Sandbox, Local).
#[derive(Debug, Clone, Default)]
pub struct WorkspaceModeHitRects {
    pub options: [Option<Rect>; 2],
    pub row: Option<Rect>,
}

/// Rows reserved above the welcome menu for the picker (content + gap).
pub const WORKSPACE_MODE_MENU_ROWS: u16 = 2;

/// Paint the segmented workspace control into `area`.
///
/// Layout:
/// `Workspace  [ Sandbox ]  [ Local workspace ]  ctrl+e`
/// or when locked: `Workspace  [ Local workspace ]  locked by CLI`
pub fn render_workspace_mode_picker(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    selected: WelcomeWorkspaceMode,
    mouse_pos: Option<(u16, u16)>,
    startup_locked: bool,
    ack_pending: bool,
) -> WorkspaceModeHitRects {
    if area.height == 0 || area.width < 20 {
        return WorkspaceModeHitRects::default();
    }

    let row = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };

    let label_style = Style::default().fg(theme.gray);
    let key_style = Style::default().fg(theme.gray_bright);
    let inactive = Style::default().fg(theme.gray_bright);
    let active = Style::default()
        .fg(theme.bg_base)
        .bg(theme.accent_user)
        .add_modifier(Modifier::BOLD);
    let hover = Style::default()
        .fg(theme.text_primary)
        .add_modifier(Modifier::BOLD);
    let locked_style = Style::default().fg(theme.gray);

    buf.set_span(row.x, row.y, &Span::styled("Workspace  ", label_style), 11);

    let mut x = row.x.saturating_add(11);
    let mut options = [None; 2];

    let modes: &[WelcomeWorkspaceMode] = if startup_locked {
        // Locked: show the effective mode only (CLI/env stamp).
        match selected {
            WelcomeWorkspaceMode::LocalWorkspace => &[WelcomeWorkspaceMode::LocalWorkspace],
            WelcomeWorkspaceMode::Sandbox => &[WelcomeWorkspaceMode::Sandbox],
        }
    } else {
        &WelcomeWorkspaceMode::ALL
    };

    for (slot, mode) in modes.iter().enumerate() {
        if x >= row.x + row.width {
            break;
        }
        let text = if *mode == selected {
            format!(" • {} ", mode.label())
        } else {
            format!(" {} ", mode.label())
        };
        let w = UnicodeWidthStr::width(text.as_str()) as u16;
        if x + w > row.x + row.width {
            break;
        }
        let rect = Rect {
            x,
            y: row.y,
            width: w,
            height: 1,
        };
        let hovered = !startup_locked
            && !ack_pending
            && mouse_pos.is_some_and(|(mx, my)| rect.contains(Position::new(mx, my)));
        let style = if *mode == selected {
            active
        } else if hovered {
            hover
        } else {
            inactive
        };
        buf.set_span(x, row.y, &Span::styled(text, style), w);
        if slot < options.len() {
            // Map by mode index so hit-test stays stable.
            options[mode.index()] = Some(rect);
        }
        x = x.saturating_add(w);
        if slot + 1 < modes.len() && x + 1 < row.x + row.width {
            buf.set_span(x, row.y, &Span::styled("│", label_style), 1);
            x = x.saturating_add(1);
        }
    }

    let trailing = if ack_pending {
        "  confirm local workspace? y/N"
    } else if startup_locked {
        "  locked by CLI"
    } else {
        "  ctrl+e"
    };
    let trailing_style = if ack_pending {
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD)
    } else if startup_locked {
        locked_style
    } else {
        key_style
    };
    if !trailing.is_empty() && x + trailing.len() as u16 <= row.x + row.width {
        buf.set_span(
            row.x + row.width - trailing.len() as u16,
            row.y,
            &Span::styled(trailing, trailing_style),
            trailing.len() as u16,
        );
    } else if ack_pending && row.width > 20 {
        // Narrow terminals: paint confirm over the right side so it stays visible.
        let short = "  y/N confirm local";
        let start = row.x + row.width.saturating_sub(short.len() as u16);
        buf.set_span(
            start,
            row.y,
            &Span::styled(short, trailing_style),
            short.len() as u16,
        );
    }

    WorkspaceModeHitRects {
        options,
        row: Some(row),
    }
}

/// Hit-test a click against option rects. Returns the selected mode if hit.
pub fn hit_test_workspace_mode(
    rects: &WorkspaceModeHitRects,
    column: u16,
    row: u16,
) -> Option<WelcomeWorkspaceMode> {
    let pos = Position::new(column, row);
    for (i, rect) in rects.options.iter().enumerate() {
        if rect.is_some_and(|r| r.contains(pos)) {
            return Some(WelcomeWorkspaceMode::from_index(i));
        }
    }
    None
}

/// Result of preparing welcome workspace intent for a new session.
#[cfg(feature = "local-workspace")]
#[derive(Debug)]
pub enum WelcomeWorkspacePrepare {
    /// Continue. `session_override`: `Some(None)` sandbox, `Some(Some)` local, `None` keep stamp.
    Continue {
        session_override: Option<Option<crate::app::session_startup::LocalWorkspaceConfig>>,
        warning: Option<String>,
    },
    /// Stay on welcome; show in-TUI ACK confirm before stamping Local.
    AwaitAck,
}

/// Prepare welcome Sandbox/Local for NewSession. Local may return `AwaitAck`.
#[cfg(feature = "local-workspace")]
pub fn prepare_welcome_workspace_for_new_session(
    selection: WelcomeWorkspaceMode,
    startup_locked: bool,
    chat_mode: bool,
    cwd: &std::path::Path,
    agents_alive: bool,
) -> anyhow::Result<WelcomeWorkspacePrepare> {
    use crate::app::session_startup::{
        local_workspace_ack_satisfied, resolve_local_workspace_config, set_active_local_workspace,
    };

    if startup_locked || !chat_mode {
        if startup_locked {
            log_cli_lock_wins(selection);
        }
        return Ok(WelcomeWorkspacePrepare::Continue {
            session_override: None,
            warning: None,
        });
    }

    match selection {
        WelcomeWorkspaceMode::Sandbox => {
            if !agents_alive {
                // Safe: no live session still reading the process stamp.
                set_active_local_workspace(None)?;
            }
            log_welcome_intent_applied(
                selection,
                startup_locked,
                "sandbox_none",
                if agents_alive { "kept" } else { "cleared" },
            );
            Ok(WelcomeWorkspacePrepare::Continue {
                session_override: Some(None),
                warning: None,
            })
        }
        WelcomeWorkspaceMode::LocalWorkspace => {
            if !local_workspace_ack_satisfied() {
                tracing::info!(
                    target: WORKSPACE_MODE_LOG,
                    event = "welcome_local_ack",
                    outcome = "await",
                    "welcome Local requires ACK confirm"
                );
                return Ok(WelcomeWorkspacePrepare::AwaitAck);
            }
            let cfg = resolve_local_workspace_config(true, Some(None), None, Some(cwd))?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "local-workspace resolve returned no config after own-mode request"
                    )
                })?;
            // Live sessions still read the process stamp; oneshot-only then.
            if !agents_alive {
                set_active_local_workspace(Some(cfg.clone()))?;
            }
            log_welcome_intent_applied(
                selection,
                startup_locked,
                "own_oneshot",
                if agents_alive { "kept" } else { "stamped_own" },
            );
            Ok(WelcomeWorkspacePrepare::Continue {
                session_override: Some(Some(cfg)),
                warning: None,
            })
        }
    }
}

/// Confirm Local ACK. If `agents_alive`, return oneshot only (keep process stamp).
#[cfg(feature = "local-workspace")]
pub fn confirm_welcome_local_workspace_ack(
    cwd: &std::path::Path,
    agents_alive: bool,
) -> anyhow::Result<crate::app::session_startup::LocalWorkspaceConfig> {
    use crate::app::session_startup::{
        resolve_local_workspace_config, set_active_local_workspace, write_local_workspace_ack,
    };

    let cfg = resolve_local_workspace_config(true, Some(None), None, Some(cwd))?
        .ok_or_else(|| anyhow::anyhow!("local-workspace resolve returned no config after ack"))?;
    if !agents_alive {
        set_active_local_workspace(Some(cfg.clone()))?;
    }
    write_local_workspace_ack();
    log_welcome_ack("confirmed");
    Ok(cfg)
}

/// Sync UI selection from a startup-locked stamp (Own/Attach → Local).
#[cfg(feature = "local-workspace")]
pub fn mode_from_active_stamp(
    stamp: Option<&crate::app::session_startup::LocalWorkspaceConfig>,
) -> WelcomeWorkspaceMode {
    match stamp {
        Some(_) => WelcomeWorkspaceMode::LocalWorkspace,
        None => WelcomeWorkspaceMode::Sandbox,
    }
}

/// Whether keyboard/mouse should mutate the welcome selection.
///
/// Same surface as ACK + render: chat mode, access, auth Done, not ZDR,
/// not CLI-startup-locked, and history picker closed (Ctrl+E/click would
/// otherwise mutate with no on-screen control).
pub fn picker_interactive(
    chat_mode: bool,
    has_access: bool,
    auth_done: bool,
    zdr_blocked: bool,
    session_picker_open: bool,
    startup_locked: bool,
) -> bool {
    chat_mode && has_access && auth_done && !zdr_blocked && !startup_locked && !session_picker_open
}

/// Center the picker within `menu_area` the same way the menu is inset.
pub fn picker_area(menu_area: Rect) -> Rect {
    let [_, centered, _] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(menu_area.width),
        Constraint::Min(0),
    ])
    .flex(Flex::Start)
    .areas(menu_area);
    Rect {
        height: 1.min(centered.height),
        ..centered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_walks_sandbox_and_local() {
        let mut mode = WelcomeWorkspaceMode::Sandbox;
        mode = mode.cycle_next();
        assert_eq!(mode, WelcomeWorkspaceMode::LocalWorkspace);
        mode = mode.cycle_next();
        assert_eq!(mode, WelcomeWorkspaceMode::Sandbox);
        assert_eq!(
            WelcomeWorkspaceMode::LocalWorkspace.cycle_prev(),
            WelcomeWorkspaceMode::Sandbox
        );
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(WelcomeWorkspaceMode::Sandbox.label(), "Sandbox");
        assert_eq!(
            WelcomeWorkspaceMode::LocalWorkspace.label(),
            "Local workspace"
        );
        assert!(
            WelcomeWorkspaceMode::LocalWorkspace
                .hint()
                .contains("Computer Hub")
        );
        assert_eq!(WelcomeWorkspaceMode::Sandbox.status_label(false), "Sandbox");
        assert_eq!(
            WelcomeWorkspaceMode::LocalWorkspace.status_label(false),
            "Local"
        );
        assert_eq!(
            WelcomeWorkspaceMode::LocalWorkspace.status_label(true),
            "Local·CLI"
        );
        assert_eq!(WelcomeWorkspaceMode::Sandbox.history_kind_filter(), "chat");
        assert_eq!(
            WelcomeWorkspaceMode::LocalWorkspace.history_kind_filter(),
            "build"
        );
        assert_eq!(
            WelcomeWorkspaceMode::from_history_source("conversation"),
            WelcomeWorkspaceMode::Sandbox
        );
        assert_eq!(
            WelcomeWorkspaceMode::from_history_source("local"),
            WelcomeWorkspaceMode::LocalWorkspace
        );
    }

    #[test]
    fn index_roundtrip() {
        for mode in WelcomeWorkspaceMode::ALL {
            assert_eq!(WelcomeWorkspaceMode::from_index(mode.index()), mode);
        }
    }

    #[test]
    fn hit_test_prefers_option_rects() {
        let rects = WorkspaceModeHitRects {
            options: [Some(Rect::new(10, 5, 9, 1)), Some(Rect::new(20, 5, 17, 1))],
            row: Some(Rect::new(0, 5, 80, 1)),
        };
        assert_eq!(
            hit_test_workspace_mode(&rects, 12, 5),
            Some(WelcomeWorkspaceMode::Sandbox)
        );
        assert_eq!(
            hit_test_workspace_mode(&rects, 25, 5),
            Some(WelcomeWorkspaceMode::LocalWorkspace)
        );
        assert_eq!(hit_test_workspace_mode(&rects, 0, 5), None);
        assert_eq!(hit_test_workspace_mode(&rects, 12, 6), None);
    }

    #[test]
    fn render_assigns_option_rects() {
        let area = Rect::new(0, 0, 100, 2);
        let mut buf = Buffer::empty(area);
        let theme = Theme::current();
        let hits = render_workspace_mode_picker(
            area,
            &mut buf,
            &theme,
            WelcomeWorkspaceMode::LocalWorkspace,
            None,
            false,
            false,
        );
        assert!(hits.options[0].is_some());
        assert!(hits.options[1].is_some());
        assert!(hits.row.is_some());
        let cell = buf.cell((0, 0)).expect("cell");
        assert_eq!(cell.symbol(), "W");
        let selected = hits.options[1].expect("local selected rect");
        let selected_text = format!(" • {} ", WelcomeWorkspaceMode::LocalWorkspace.label());
        assert_eq!(
            selected.width,
            UnicodeWidthStr::width(selected_text.as_str()) as u16,
            "option width must be display columns, not UTF-8 bytes"
        );
        assert!(
            selected.width < selected_text.len() as u16,
            "bullet U+2022 is 3 bytes / 1 column: {selected_text:?}"
        );
    }

    #[test]
    fn render_ack_pending_shows_durable_confirm() {
        let area = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(area);
        let theme = Theme::current();
        let _ = render_workspace_mode_picker(
            area,
            &mut buf,
            &theme,
            WelcomeWorkspaceMode::LocalWorkspace,
            None,
            false,
            true,
        );
        let line: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(
            line.contains("y/N") || line.contains("confirm"),
            "ack-pending UI must stay visible: {line:?}"
        );
    }

    #[test]
    fn picker_interactive_matrix() {
        assert!(picker_interactive(true, true, true, false, false, false));
        assert!(!picker_interactive(true, true, true, false, false, true));
        assert!(
            !picker_interactive(true, true, true, false, true, false),
            "history open: Ctrl+E/click must not mutate a hidden control"
        );
        assert!(!picker_interactive(true, false, true, false, false, false));
        assert!(!picker_interactive(false, true, true, false, false, false));
        assert!(
            !picker_interactive(true, true, false, false, false, false),
            "login / authenticating must not cycle mode"
        );
        assert!(
            !picker_interactive(true, true, true, true, false, false),
            "ZDR-blocked welcome must not cycle mode"
        );
    }

    #[test]
    fn indicator_derives_from_opened_session() {
        assert_eq!(
            indicator_for_opening_session(true, false, false, false),
            (WelcomeWorkspaceMode::Sandbox, false)
        );
        assert_eq!(
            indicator_for_opening_session(false, true, false, false),
            (WelcomeWorkspaceMode::LocalWorkspace, false)
        );
        // Conversation / chat_kind without this-session local intent → Sandbox
        // even when the process has a CLI lock (LoadSession strips stamp).
        assert_eq!(
            indicator_for_opening_session(true, false, true, false),
            (WelcomeWorkspaceMode::Sandbox, false)
        );
        assert_eq!(
            indicator_for_opening_session(true, false, true, true),
            (WelcomeWorkspaceMode::LocalWorkspace, true)
        );
        assert_eq!(
            indicator_for_opening_session(false, true, true, false),
            (WelcomeWorkspaceMode::LocalWorkspace, true)
        );
        assert_eq!(WelcomeWorkspaceMode::Sandbox.status_label(true), "Sandbox");
    }
}

#[cfg(all(test, feature = "local-workspace"))]
mod apply_tests {
    use super::*;
    use crate::app::session_startup::{
        ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK_ENV, LocalWorkspaceMode, set_active_local_workspace,
    };

    #[test]
    fn startup_lock_skips_override() {
        set_active_local_workspace(None).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        set_active_local_workspace(Some(crate::app::session_startup::LocalWorkspaceConfig {
            mode: LocalWorkspaceMode::Attach,
            cwd: Some(tmp.path().to_path_buf()),
            server_id: Some("cli-srv".into()),
        }))
        .unwrap();

        let out = prepare_welcome_workspace_for_new_session(
            WelcomeWorkspaceMode::Sandbox,
            true,
            true,
            tmp.path(),
            false,
        )
        .unwrap();
        match out {
            WelcomeWorkspacePrepare::Continue {
                session_override, ..
            } => {
                assert!(session_override.is_none());
            }
            WelcomeWorkspacePrepare::AwaitAck => panic!("locked must continue"),
        }
        let stamp = crate::app::session_startup::active_local_workspace()
            .unwrap()
            .expect("cli stamp kept");
        assert_eq!(stamp.mode, LocalWorkspaceMode::Attach);
        set_active_local_workspace(None).unwrap();
    }

    #[test]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK)]
    fn welcome_local_one_shot_only_when_agents_alive() {
        let _ack =
            echo_agent_test_support::EnvGuard::set(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK_ENV, "1");
        set_active_local_workspace(None).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let out = prepare_welcome_workspace_for_new_session(
            WelcomeWorkspaceMode::LocalWorkspace,
            false,
            true,
            tmp.path(),
            true, // agents alive
        )
        .unwrap();
        let WelcomeWorkspacePrepare::Continue {
            session_override, ..
        } = out
        else {
            panic!("expected continue");
        };
        assert!(session_override.flatten().is_some());
        assert!(
            crate::app::session_startup::active_local_workspace()
                .unwrap()
                .is_none(),
            "must not overwrite process stamp while other agents are alive"
        );
    }

    #[test]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK)]
    fn welcome_local_stamps_own_mode() {
        let _ack =
            echo_agent_test_support::EnvGuard::set(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK_ENV, "1");
        set_active_local_workspace(None).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let out = prepare_welcome_workspace_for_new_session(
            WelcomeWorkspaceMode::LocalWorkspace,
            false,
            true,
            tmp.path(),
            false,
        )
        .unwrap();
        let WelcomeWorkspacePrepare::Continue {
            session_override, ..
        } = out
        else {
            panic!("expected continue");
        };
        let cfg = session_override.flatten().expect("own stamp override");
        assert_eq!(cfg.mode, LocalWorkspaceMode::Own);
        assert_eq!(cfg.cwd.as_deref(), Some(tmp.path()));
        assert!(cfg.server_id.is_none());
        set_active_local_workspace(None).unwrap();
    }

    #[test]
    fn sandbox_does_not_clear_stamp_when_agents_alive() {
        set_active_local_workspace(None).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        set_active_local_workspace(Some(crate::app::session_startup::LocalWorkspaceConfig {
            mode: LocalWorkspaceMode::Own,
            cwd: Some(tmp.path().to_path_buf()),
            server_id: None,
        }))
        .unwrap();

        let out = prepare_welcome_workspace_for_new_session(
            WelcomeWorkspaceMode::Sandbox,
            false,
            true,
            tmp.path(),
            true, // agents alive
        )
        .unwrap();
        let WelcomeWorkspacePrepare::Continue {
            session_override, ..
        } = out
        else {
            panic!("expected continue");
        };
        assert_eq!(session_override, Some(None));
        assert!(
            crate::app::session_startup::active_local_workspace()
                .unwrap()
                .is_some(),
            "process stamp must remain for live agents"
        );
        set_active_local_workspace(None).unwrap();
    }

    #[test]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK)]
    fn local_without_ack_awaits_confirm() {
        let _ack =
            echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK_ENV);
        // Isolate ack file from developer machine.
        let home = tempfile::tempdir().unwrap();
        let _home = echo_agent_test_support::EnvGuard::set(
            "ECHO_AGENT_HOME",
            home.path().to_str().unwrap(),
        );
        set_active_local_workspace(None).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let out = prepare_welcome_workspace_for_new_session(
            WelcomeWorkspaceMode::LocalWorkspace,
            false,
            true,
            tmp.path(),
            false,
        )
        .unwrap();
        assert!(matches!(out, WelcomeWorkspacePrepare::AwaitAck));
        assert!(
            crate::app::session_startup::active_local_workspace()
                .unwrap()
                .is_none()
        );
    }
}
