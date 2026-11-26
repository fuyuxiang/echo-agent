//! Transcript export, block copying, viewer/modal, and input-log dump dispatchers.

use super::ctx::with_active_agent;
use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::app_view::{ActiveView, AppView};
use crate::scrollback::block::{BlockContent, RenderBlock};
use crate::scrollback::blocks::ToolCallBlock;
use agent_client_protocol as acp;
use echo_agent_telemetry::session_ctx::log_event;

/// Copy the selected block's content to the system clipboard.
///
/// Respects the block's raw/pretty mode for markdown content.
/// Shows a toast notification on theExtensionsTab
pub(super) fn dispatch_copy_block_content(app: &mut AppView) {
    with_active_agent(app, |agent| {
        let Some(idx) = agent.scrollback.selected() else {
            return;
        };
        if agent.scrollback.entry_content_hidden_by_group(idx) {
            return;
        }
        let Some(entry) = agent.scrollback.entry(idx) else {
            return;
        };

        // BgTask blocks: copy stdout from central store
        let text = if let RenderBlock::BgTask(block) = &entry.block {
            let stdout = agent
                .session
                .bg_tasks
                .get(&block.task_id)
                .map(|t| t.stdout.clone())
                .unwrap_or_default();
            if stdout.is_empty() {
                None
            } else {
                Some(stdout)
            }
        } else {
            entry.block.copy_text(entry.raw)
        };

        if let Some(text) = text
            && !text.is_empty()
        {
            agent.copy_to_clipboard(&text);
        }
    });
}

/// Copy the Nth most recent assistant message to the clipboard, or to `file_path`.
pub(super) fn dispatch_copy_assistant_message(
    app: &mut AppView,
    n: usize,
    file_path: Option<std::path::PathBuf>,
) {
    with_active_agent(app, |agent| {
        // Collect agent messages in reverse order (most recent first).
        let mut agent_messages: Vec<String> = Vec::new();
        for i in (0..agent.scrollback.len()).rev() {
            if let Some(entry) = agent.scrollback.entry(i)
                && let RenderBlock::AgentMessage(msg) = &entry.block
            {
                agent_messages.push(msg.copy_text(true));
            }
        }

        if agent_messages.is_empty() {
            agent
                .scrollback
                .push_block(RenderBlock::system("No assistant messages to copy"));
            return;
        }

        if n > agent_messages.len() {
            agent.scrollback.push_block(RenderBlock::system(format!(
                "Only {} assistant {} available to copy",
                agent_messages.len(),
                if agent_messages.len() == 1 {
                    "message"
                } else {
                    "messages"
                }
            )));
            return;
        }

        let text = &agent_messages[n - 1];
        if text.is_empty() {
            agent
                .scrollback
                .push_block(RenderBlock::system("Assistant message is empty"));
            return;
        }

        let stats = crate::clipboard::clipboard_stats_suffix(text);

        if let Some(p) = file_path {
            match crate::clipboard::write_text_to_copy_file(text, &p) {
                Ok(path) => {
                    agent.scrollback.push_block(RenderBlock::system(format!(
                        "Copied to {}{stats}",
                        path.display()
                    )));
                }
                Err(e) => {
                    agent
                        .scrollback
                        .push_block(RenderBlock::system(format!("Failed to write file: {e}")));
                }
            }
            return;
        }

        let delivery = crate::clipboard::copy_text_or_file(text);
        match &delivery {
            crate::clipboard::CopyDelivery::Clipboard { file, .. } => {
                let block_msg = match file {
                    Some(path) => format!(
                        "Copied to clipboard (also saved to {}){stats}",
                        crate::clipboard::display_copy_path(path)
                    ),
                    None => format!("Copied to clipboard{stats}"),
                };
                agent.scrollback.push_block(RenderBlock::system(block_msg));
            }
            crate::clipboard::CopyDelivery::File { path } => {
                agent.scrollback.push_block(RenderBlock::system(format!(
                    "Clipboard unreachable: wrote {}{stats}",
                    crate::clipboard::display_copy_path(path)
                )));
            }
            crate::clipboard::CopyDelivery::Failed { .. } => {
                agent
                    .scrollback
                    .push_block(RenderBlock::system(format!("Copy failed{stats}")));
            }
        }
        agent.show_toast_ticks(delivery.toast_message().as_ref(), delivery.toast_ticks());
    });
}

/// Dispatch for the `/export` command.
/// Collects the active (sub)agent's scrollback, renders a clean Markdown transcript,
/// and either writes it to the (expanded) file or copies it to the clipboard using the
/// full route (native + tmux + OSC 52) with appropriate feedback.
pub(super) fn dispatch_export_conversation(
    app: &mut AppView,
    file_path: Option<std::path::PathBuf>,
) {
    with_active_agent(app, |agent| {
        let blocks: Vec<_> = (0..agent.scrollback.len())
            .filter_map(|i| agent.scrollback.entry(i).map(|e| &e.block))
            .collect();

        let md = crate::scrollback::export::render_blocks_to_markdown(blocks);

        if md.is_empty() {
            agent
                .scrollback
                .push_block(RenderBlock::system("No conversation content to export"));
            return;
        }

        if let Some(p) = file_path {
            // All fs logic (tilde, mkdir, write) lives here (single owner, thin command layer).
            let expanded =
                std::path::PathBuf::from(shellexpand::tilde(&p.to_string_lossy()).as_ref());
            if let Some(parent) = expanded.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                agent.scrollback.push_block(RenderBlock::system(format!(
                    "Failed to create directory: {e}"
                )));
                return;
            }
            match std::fs::write(&expanded, &md) {
                Ok(()) => {
                    agent.scrollback.push_block(RenderBlock::system(format!(
                        "Conversation exported to {}",
                        expanded.display()
                    )));
                }
                Err(e) => {
                    // Do not blindly re-emit a user-supplied path in the error message
                    // (it may contain secrets or PII); the generic failure is sufficient.
                    agent
                        .scrollback
                        .push_block(RenderBlock::system(format!("Failed to write file: {}", e)));
                }
            }
        } else {
            // Clipboard path: stats block (like assistant copy) + route-aware toast
            // (like block content copy / selection). Good UX for a potentially large transcript.
            // The scrollback line reflects where the copy actually landed —
            // same pattern as /copy N — instead of claiming clipboard success
            // when the delivery fell back to the backup file.
            let stats = crate::clipboard::clipboard_stats_suffix(&md);
            let delivery = agent.copy_to_clipboard(&md);
            let block_msg = match &delivery {
                crate::clipboard::CopyDelivery::Clipboard { file, .. } => match file {
                    Some(path) => format!(
                        "Conversation copied to clipboard (also saved to {}){stats}",
                        crate::clipboard::display_copy_path(path)
                    ),
                    None => format!("Conversation copied to clipboard{stats}"),
                },
                crate::clipboard::CopyDelivery::File { path } => format!(
                    "Clipboard unreachable: conversation written to {}{stats}",
                    crate::clipboard::display_copy_path(path)
                ),
                crate::clipboard::CopyDelivery::Failed { .. } => {
                    format!("Conversation copy failed{stats}")
                }
            };
            agent.scrollback.push_block(RenderBlock::system(block_msg));
        }
    });
}

/// Open the full transcript in `$PAGER`.
///
/// **Minimal mode** renders a full-fidelity ANSI transcript — every block
/// fully expanded (reasoning in full, tool output uncapped, diff colors kept)
/// — a full layout + syntax-highlight + ANSI-serialization pass over the whole
/// session. Rendering that inline froze the event loop for seconds on long
/// sessions ("laggy /transcript"), and the block model is `!Send` (syntect's
/// resumable highlighter state lives inside markdown blocks), so it can't be
/// shipped to a worker either. Instead this only ARMS the request; the minimal
/// render loop builds the transcript **incrementally, a time-budgeted slice
/// per frame** (`full_view::pump_transcript`, the same time-sliced amortization
/// pattern other TUIs use for heavy transcript work), then arms `pending_pager_path`
/// for the event loop's suspend-into-`$PAGER`.
///
/// **Other modes** keep the compact markdown export (string concatenation, no
/// layout or highlighting — cheap enough to stay synchronous).
pub(crate) fn dispatch_open_transcript_pager(app: &mut AppView) {
    if app.screen_mode.is_minimal() {
        crate::minimal_api::request_minimal_transcript(app);
        return;
    }

    let mut md = None;
    with_active_agent(app, |agent| {
        let blocks: Vec<_> = (0..agent.scrollback.len())
            .filter_map(|i| agent.scrollback.entry(i).map(|e| &e.block))
            .collect();
        let rendered = crate::scrollback::export::render_blocks_to_markdown(blocks);
        if !rendered.is_empty() {
            md = Some(rendered);
        }
    });

    let Some(content) = md else {
        with_active_agent(app, |agent| {
            agent.scrollback.push_block(RenderBlock::system(
                "No conversation transcript to view yet",
            ));
        });
        return;
    };

    let path =
        std::env::temp_dir().join(format!("echo-agent-transcript-{}.md", uuid::Uuid::new_v4()));
    match std::fs::write(&path, content) {
        Ok(()) => {
            app.pending_pager_path = Some(path);
            app.pending_pager_ansi = false;
        }
        Err(e) => {
            with_active_agent(app, |agent| {
                agent.scrollback.push_block(RenderBlock::system(format!(
                    "Failed to write transcript: {e}"
                )));
            });
        }
    }
}

/// Open the fullscreen block viewer for the selected entry.
/// Falls back to the image viewer only for entries without a normal block viewer.
pub(super) fn dispatch_open_block_viewer(app: &mut AppView) {
    use crate::views::block_viewer::BlockViewerPane;

    with_active_agent(app, |agent| {
        let Some(idx) = agent.scrollback.selected() else {
            return;
        };
        let Some(entry) = agent.scrollback.entry(idx) else {
            return;
        };

        // Block has images/media but terminal can't render pixels — toast and bail.
        let has_media =
            !entry.block.image_references().is_empty() || entry.block.inline_media().is_some();
        if has_media && !crate::terminal::image::detect_graphics_protocol().supports_images() {
            agent.guard_image_support();
            return;
        }

        if !entry.block.has_normal_fullscreen_viewer() {
            // Video: Enter starts inline playback (no modal).
            if let Some(first_ref) = entry.block.video_references().first() {
                let path = first_ref.path.clone();
                agent.start_inline_video_playback(&path);
                return;
            }
            // Image: Enter opens the file in the OS-native viewer.
            if let Some(first_ref) = entry.block.image_references().first() {
                let path = first_ref.path.clone();
                agent.open_media_natively(&path);
            }
            return;
        }

        // Try to create a normal viewer for the selected block type.
        let viewer = match &entry.block {
            RenderBlock::Thinking(_) | RenderBlock::AgentMessage(_) => {
                BlockViewerPane::for_markdown(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::Execute(_)) => {
                BlockViewerPane::for_execute(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::Edit(_)) => {
                BlockViewerPane::for_edit(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::Read(_)) => {
                BlockViewerPane::for_read(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::Search(_)) => {
                BlockViewerPane::for_grep(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::ListDir(_)) => {
                BlockViewerPane::for_list_dir(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::WebFetch(_)) => {
                BlockViewerPane::for_web_fetch(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::WebSearch(_)) => {
                BlockViewerPane::for_web_search(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::IntegrationSearch(_)) => {
                BlockViewerPane::for_integration_search(entry.id, entry)
            }
            RenderBlock::ToolCall(ToolCallBlock::UseTool(_)) => {
                BlockViewerPane::for_use_tool(entry.id, entry)
            }
            RenderBlock::BgTask(block) => {
                let stdout = agent
                    .session
                    .bg_tasks
                    .get(&block.task_id)
                    .map(|t| t.stdout.as_str())
                    .unwrap_or("");
                let is_running = agent
                    .session
                    .bg_tasks
                    .get(&block.task_id)
                    .is_some_and(|t| t.status == crate::app::agent::BgTaskStatus::Running);
                Some(BlockViewerPane::for_bg_task(
                    entry.id,
                    &block.task_id,
                    stdout,
                    is_running,
                ))
            }
            _ => None,
        };

        if viewer.is_some() {
            agent.block_viewer = viewer;
            return;
        }

        // Video: Enter starts inline playback.
        if let Some(first_ref) = entry.block.video_references().first() {
            let path = first_ref.path.clone();
            agent.start_inline_video_playback(&path);
            return;
        }
        // Image: Enter opens the file in the OS-native viewer.
        if let Some(first_ref) = entry.block.image_references().first() {
            let path = first_ref.path.clone();
            agent.open_media_natively(&path);
        }
    });
}

/// Fetch-set that populates every Extensions-modal tab. Shared by the manual
/// open path, the post-CTA-install auth handoff, and the deferred-fetch
/// session-ready handlers so they can't drift and leave a tab stuck on its
/// initial `Loading` state.
pub(super) fn extensions_modal_tab_fetches(
    modal: &mut crate::views::extensions_modal::ExtensionsModalState,
    agent_id: AgentId,
    session_id: acp::SessionId,
) -> Vec<Effect> {
    let mut effects = vec![
        Effect::FetchHooksList {
            agent_id,
            session_id: session_id.clone(),
        },
        Effect::FetchPluginsList {
            agent_id,
            session_id: session_id.clone(),
        },
        Effect::FetchMcpsList {
            agent_id,
            session_id: session_id.clone(),
            cache: true,
        },
        Effect::FetchSkillsList {
            agent_id,
            session_id: session_id.clone(),
        },
        Effect::FetchWorkflowsList {
            agent_id,
            session_id: session_id.clone(),
        },
    ];
    push_marketplace_fetch(modal, &mut effects, agent_id, session_id);
    effects
}

/// Push a marketplace list fetch, coalescing overlapping requests: while one
/// is in flight, further requests fold into a single queued refetch that
/// fires when the current response lands (see the field docs on
/// `ExtensionsModalState`). The other tab fetches are cheap local reads and
/// don't need this.
pub(super) fn push_marketplace_fetch(
    modal: &mut crate::views::extensions_modal::ExtensionsModalState,
    effects: &mut Vec<Effect>,
    agent_id: AgentId,
    session_id: acp::SessionId,
) {
    if modal.marketplace_fetch_inflight {
        modal.marketplace_refetch_queued = true;
        return;
    }
    modal.marketplace_fetch_inflight = true;
    effects.push(Effect::FetchMarketplaceList {
        agent_id,
        session_id,
    });
}

/// Slash-command name for an extensions modal tab (toast when not on an agent).
fn extensions_tab_slash_name(tab: crate::views::extensions_modal::ExtensionsTab) -> &'static str {
    use crate::views::extensions_modal::ExtensionsTab;
    match tab {
        ExtensionsTab::Hooks => "hooks",
        ExtensionsTab::Plugins => "plugins",
        ExtensionsTab::Marketplace => "marketplace",
        ExtensionsTab::Skills => "skills",
        ExtensionsTab::Workflows => "workflows",
        ExtensionsTab::McpServers => "mcps",
    }
}

/// Slash-command name for the config-agents modal tab.
fn config_agents_slash_name(tab: Option<crate::views::agents_modal::AgentsTab>) -> &'static str {
    use crate::views::agents_modal::AgentsTab;
    match tab {
        Some(AgentsTab::Personas) => "personas",
        Some(AgentsTab::Agents) | None => "config-agents",
    }
}

/// Toast when a session-hosted modal is opened off the agent view.
fn toast_session_only_slash(app: &mut AppView, name: &str) {
    let msg = format!("/{name} only works in a session. Open an agent first.");
    match app.active_view {
        ActiveView::AgentDashboard => {
            if let Some(d) = app.dashboard.as_mut() {
                d.set_error_toast(&msg);
            }
        }
        ActiveView::Welcome | ActiveView::Agent(_) => {
            app.show_toast(&msg);
        }
    }
}

/// Open the hooks/plugins modal on the active agent view and fetch list data.
pub(super) fn dispatch_open_extensions_modal(
    app: &mut AppView,
    tab: crate::views::extensions_modal::ExtensionsTab,
    trigger: echo_agent_telemetry::events::ExtensionsModalTrigger,
) -> Vec<Effect> {
    use crate::views::extensions_modal::ExtensionsModalState;

    let ActiveView::Agent(id) = app.active_view else {
        toast_session_only_slash(app, extensions_tab_slash_name(tab));
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    // Mutual exclusivity: close agents modal when opening extensions.
    agent.agents_modal = None;
    let mut modal = ExtensionsModalState::new(tab);
    modal.session_team_id = app.team_id.clone();
    agent.extensions_modal = Some(modal);
    log_event(echo_agent_telemetry::events::ExtensionsModalOpened {
        trigger,
        tab: tab.telemetry_tab(),
    });

    let Some(session_id) = agent.session.session_id.clone() else {
        // Tabs default to Loading; the fetch fires on SessionCreated.
        agent.pending_extensions_fetch = true;
        return vec![];
    };
    agent.pending_extensions_fetch = false;
    let Some(modal) = agent.extensions_modal.as_mut() else {
        return vec![];
    };
    extensions_modal_tab_fetches(modal, id, session_id)
}

/// Open the agents modal, showing all agent definitions.
pub(super) fn dispatch_open_config_agents_modal(
    app: &mut AppView,
    initial_tab: Option<crate::views::agents_modal::AgentsTab>,
) -> Vec<Effect> {
    use crate::views::agents_modal::{AgentsModalState, load_agent_toggle};

    let ActiveView::Agent(id) = app.active_view else {
        toast_session_only_slash(app, config_agents_slash_name(initial_tab));
        return vec![];
    };
    let bundle = app.bundle_state.clone();
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    // Mutual exclusivity with extensions_modal
    agent.extensions_modal = None;

    let cwd = agent.session.cwd.clone();
    let toggle = load_agent_toggle();
    let model_agent_type = agent
        .session
        .models
        .current
        .as_ref()
        .and_then(|id| agent.session.models.available.get(id))
        .and_then(model_agent_type_from_info);
    let session_id = agent.session.session_id.clone();
    let active_agent = agent.session_agent_name.clone();
    // One-shot plugin discovery (same gating as `/mcp doctor` and `inspect`)
    // so plugin-provided agents are listed alongside native ones.
    let plugin_registry = echo_agent_runtime::util::config::load_cli_plugin_registry(&cwd);
    let plugin_registry = (!plugin_registry.is_empty()).then_some(plugin_registry);
    let mut modal = AgentsModalState::new(
        &cwd,
        &toggle,
        &bundle,
        model_agent_type.as_deref(),
        active_agent,
        plugin_registry,
    );
    if let Some(tab) = initial_tab {
        modal.active_tab = tab;
    }
    agent.agents_modal = Some(modal);
    if let Some(session_id) = session_id {
        return vec![Effect::FetchSessionAgentName {
            agent_id: id,
            session_id,
        }];
    }
    vec![]
}

/// `agentType` / `agent_type` from a catalog `ModelInfo` meta blob.
fn model_agent_type_from_info(info: &agent_client_protocol::ModelInfo) -> Option<String> {
    let meta = info.meta.as_ref()?;
    ["agentType", "agent_type"]
        .into_iter()
        .find_map(|key| meta.get(key))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Copy the selected block's metadata (e.g., command) to clipboard.
pub(super) fn dispatch_copy_block_meta(app: &mut AppView) {
    with_active_agent(app, |agent| {
        let Some(idx) = agent.scrollback.selected() else {
            return;
        };
        if agent.scrollback.entry_content_hidden_by_group(idx) {
            return;
        }
        let Some(entry) = agent.scrollback.entry(idx) else {
            return;
        };
        if let Some(text) = entry.block.copy_meta()
            && !text.is_empty()
        {
            agent.copy_to_clipboard(&text);
        }
    });
}

/// Dump the input flight recorder to a JSON file for debugging.
/// See `input_log.rs` module docs for lifecycle/removal instructions.
pub(super) fn dispatch_dump_input_log(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    if agent.input_log.entry_count() == 0 {
        agent.show_toast("No input events recorded yet.");
        return vec![];
    }

    let time_span_ms = agent.input_log.time_span_ms();
    let entries = agent.input_log.snapshot_entries();
    let entry_count = entries.len();
    let terminal = crate::terminal::terminal_context().telemetry_snapshot();
    let session_id = agent.session.session_id.as_ref().map(|s| s.0.to_string());
    let pager_version = crate::client_identity::PAGER_CLIENT_VERSION;

    let now = chrono::Utc::now();
    let dump = crate::input_log::InputDump {
        dumped_at: now.to_rfc3339(),
        session_id: session_id.clone(),
        pager_version,
        terminal,
        active_pane: format!("{:?}", agent.active_pane),
        textarea_cursor: agent.prompt.cursor(),
        textarea_text_len: agent.prompt.text().len(),
        textarea_has_selection: agent.prompt.textarea.selection_range().is_some(),
        entry_count,
        time_span_ms,
        entries,
    };

    let json = match serde_json::to_string_pretty(&dump) {
        Ok(j) => j,
        Err(e) => {
            agent.show_toast(&format!("Failed to serialize input log: {e}"));
            return vec![];
        }
    };

    let echo_agent_home = echo_agent_tools::util::echo_agent_home::echo_agent_home();
    let logs_dir = echo_agent_home.join("logs");
    let _ = std::fs::create_dir_all(&logs_dir);
    let ts = now.format("%Y%m%d-%H%M%S");
    let path = logs_dir.join(format!("input-debug-{ts}.json"));

    match std::fs::write(&path, json) {
        Ok(()) => {
            let display_path = path.display();
            agent.show_toast(&format!(
                "Input log ({entry_count} events) → {display_path}"
            ));
            crate::unified_log::info(
                &format!("input debug dump: {entry_count} events, {time_span_ms}ms span"),
                session_id.as_deref(),
                None,
            );
        }
        Err(e) => {
            agent.show_toast(&format!("Failed to write input log: {e}"));
        }
    }
    vec![]
}

// TaskResult handlers.

pub(super) fn handle_hooks_list_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<echo_agent_hooks_plugins_types::HooksListResponse, String>,
) -> Vec<Effect> {
    use crate::views::extensions_modal::TabDataState;
    if let Some(agent) = app.agents.get_mut(&agent_id)
        && let Some(ref mut modal) = agent.extensions_modal
    {
        modal.hooks_data = match result {
            Ok(response) => {
                // Default all groups to collapsed.
                let mut seen = std::collections::HashSet::new();
                for hook in &response.hooks {
                    seen.insert(hook.source_dir.clone());
                }
                modal.hooks_collapsed_groups = seen;
                TabDataState::Loaded(response)
            }
            Err(e) => TabDataState::Error(e),
        };
    }
    vec![]
}

pub(super) fn handle_plugins_list_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<echo_agent_hooks_plugins_types::PluginsListResponse, String>,
) -> Vec<Effect> {
    use crate::views::extensions_modal::TabDataState;
    if let Some(agent) = app.agents.get_mut(&agent_id)
        && let Some(ref mut modal) = agent.extensions_modal
    {
        modal.plugins_data = match result {
            Ok(response) => {
                modal.seed_plugin_groups_once(&response.plugins);
                TabDataState::Loaded(response)
            }
            Err(e) => TabDataState::Error(e),
        };
        // Clear pending_action so the UI unblocks as soon as the
        // plugins list arrives. Marketplace can continue loading
        // independently via its own TabDataState::Loading.
        modal.pending_action = None;
        modal.pending_entry_index = None;
    }
    vec![]
}

pub(super) fn handle_mcp_toggle_done(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<(), String>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    if let Some(ref mut modal) = agent.extensions_modal
        && let Err(e) = result
    {
        modal.pending_action = None;
        modal.pending_entry_index = None;
        modal.modal_message = Some(crate::views::extensions_modal::ModalMessage::Error(e));
        return vec![];
    }
    let Some(session_id) = agent.session.session_id.clone() else {
        return vec![];
    };
    vec![Effect::FetchMcpsList {
        agent_id,
        session_id,
        cache: false,
    }]
}

pub(super) fn handle_marketplace_updates_available(
    app: &mut AppView,
    agent_id: AgentId,
    // (name, installed_ver, latest_ver)
    updates: Vec<(String, String, String)>,
) -> Vec<Effect> {
    if !updates.is_empty()
        && let Some(agent) = app.agents.get_mut(&agent_id)
    {
        let names: Vec<String> = updates
            .iter()
            .map(|(name, old, new)| format!("{name} (v{old} \u{2192} v{new})"))
            .collect();
        let summary = if names.len() <= 2 {
            names.join(", ")
        } else {
            format!("{} and {} more", names[..2].join(", "), names.len() - 2)
        };
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::system(format!(
                "{} Plugins auto-updated: {summary}.",
                crate::glyphs::diamond_filled()
            )));
    }
    vec![]
}

pub(super) fn handle_marketplace_list_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<echo_agent_hooks_plugins_types::MarketplaceListResponse, String>,
) -> Vec<Effect> {
    use crate::views::extensions_modal::TabDataState;
    let mut effects = Vec::new();
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        let session_id = agent.session.session_id.clone();
        let Some(ref mut modal) = agent.extensions_modal else {
            return effects;
        };
        modal.marketplace_fetch_inflight = false;
        if std::mem::take(&mut modal.marketplace_refetch_queued)
            && let Some(session_id) = session_id
        {
            push_marketplace_fetch(modal, &mut effects, agent_id, session_id);
        }
        modal.marketplace_data = match result {
            Ok(mut response) => {
                response.sanitize();
                // Only default to collapsed on first load (when state is Loading).
                // On reloads (after install/uninstall/refresh), preserve the user's
                // expand/collapse choices.
                let is_first_load = matches!(modal.marketplace_data, TabDataState::Loading);
                if is_first_load {
                    // All sources start collapsed.
                    modal.marketplace_collapsed_sources = (0..response.sources.len()).collect();
                }
                TabDataState::Loaded(response)
            }
            Err(e) => TabDataState::Error(e),
        };
        modal.pending_action = None;
        modal.pending_entry_index = None;
    }
    effects
}

pub(super) fn handle_skills_toggle_done(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<Vec<echo_agent_tools::implementations::skills::types::SkillInfo>, String>,
) -> Vec<Effect> {
    use crate::views::extensions_modal::TabDataState;
    if let Some(agent) = app.agents.get_mut(&agent_id)
        && let Some(ref mut modal) = agent.extensions_modal
    {
        modal.pending_action = None;
        modal.pending_entry_index = None;
        match result {
            Ok(skills) => {
                modal.seed_skills_groups_once(&skills);
                modal.skills_data = TabDataState::Loaded(skills);
            }
            Err(e) => {
                modal.modal_message = Some(crate::views::extensions_modal::ModalMessage::Error(e));
            }
        }
    }
    // The toggle effect already called echo.agent/skills/refresh-baseline
    // which triggers the session to reload skills and push an
    // AvailableCommandsUpdate notification with the updated list.
    vec![]
}
