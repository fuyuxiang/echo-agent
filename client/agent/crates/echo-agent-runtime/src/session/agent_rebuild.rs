//! `AgentRebuildSpec` — the canonical recipe for constructing an
//! [`echo_agent_core::Agent`] for a given session.
//!
//! INVARIANT: This is the **only** place in the shell crate that calls
//! [`echo_agent_core::AgentBuilder::new`]. Both initial session spawn
//! ([`crate::session::acp_session::spawn_session_actor`]) and zero-turn
//! harness rebuild
//! ([`crate::session::acp_session::SessionActor::handle_rebuild_agent_for_definition`])
//! go through [`AgentRebuildSpec::build_agent`].
//!
//! ## Why this exists
//!
//! [`echo_agent_core::Agent`] owns an [`echo_agent_tools::bridge::ToolBridge`]
//! that carries session-scoped channels (notification handle, terminal/fs
//! backends, subagent senders, scheduler set, plugin registry, attribution
//! callback). The Agent is therefore session-bound — it cannot be shared
//! across sessions and cannot be re-rendered from outside its session
//! context. To rebuild it (e.g. when the user picks a model with a
//! different `agent_type` before sending any user message), we need to
//! retain every input that the original `AgentBuilder` chain consumed.
//! `AgentRebuildSpec` is exactly that retained bag of inputs.
//!
//! ## WHEN ADDING A NEW [`echo_agent_core::AgentBuilder`]`::with_*` KNOB
//!
//! 1. Add the corresponding field to [`AgentRebuildSpec`].
//! 2. Pass it through in [`AgentRebuildSpec::build_agent`]. The destructure
//!    pattern at the top of `build_agent` forces every field to be used —
//!    drift is a compile error (`#[deny(unused_variables)]`).
//! 3. Populate the field at the call site in `spawn_session_actor`.
//!
//! ## Why some fields are channel senders
//!
//! Several `ToolBridge` resources (e.g. `UserQuestionSender`,
//! `SubagentBackendResource`) are backed by the `tx` half of channels
//! whose `rx` halves are owned by long-lived coordinator tasks spawned
//! in `spawn_session_actor`. The subagent channels are wrapped in a
//! `ChannelBackend` behind `SubagentBackendResource`. On rebuild, we
//! must reuse the **same** senders so the existing coordinator keeps
//! receiving requests; we cannot mint a fresh channel without orphaning
//! the running coordinator.
use echo_agent_core::config::AgentDefinition;
use echo_agent_core::error::AgentBuildError;
use echo_agent_core::prompt::context::PromptAudience;
use echo_agent_core::prompt::skills::SkillsConfig;
use echo_agent_core::{Agent, AgentBuilder, CompactionPolicy, ReminderPolicy};
use echo_agent_tools::computer::types::{AsyncFileSystem, TerminalBackend};
use echo_agent_tools::implementations::echo_agent_build::app_builder::AppBuilderDeployerConfig;
use echo_agent_tools::implementations::echo_agent_build::ask_user_question::types::UserQuestionRequest;
use echo_agent_tools::implementations::echo_agent_build::image_gen::ImageGenConfig;
use echo_agent_tools::implementations::echo_agent_build::monitor::types::MonitorEventBuffer;
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentEvent, TaskModelValidator,
};
use echo_agent_tools::implementations::echo_agent_build::video_gen::VideoGenConfig;
use echo_agent_tools::implementations::echo_agent_build::web_fetch::WebFetchConfig;
use echo_agent_tools::implementations::lsp::LspBackend;
use echo_agent_tools::implementations::web_search::WebSearchConfig;
use echo_agent_tools::notification::ToolNotificationHandle;
use echo_agent_tools::types::SharedApiKeyProvider;
use echo_agent_tools::types::compat::CompatConfig;
use echo_agent_tools::types::memory_backend::MemoryBackend;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
/// Shell-resolved per-tool `ToolConfig.params` JSON maps, bundled into one
/// named struct so the spawn telescopes carry a single argument instead of
/// adjacent identically-typed positionals that a caller could transpose.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedToolParamsJson {
    /// `[toolset.bash]` overrides for the bash tool(s).
    pub bash: Option<serde_json::Map<String, serde_json::Value>>,
    /// `[toolset.ask_user_question]` timeout policy for the ask tool.
    pub ask_user_question: Option<serde_json::Map<String, serde_json::Value>>,
}
/// Cached recipe for building a session-scoped [`Agent`].
///
/// See module docs for the invariant: this is the only construction
/// site for `Agent` in the shell crate. Cloning is intentionally not
/// derived — the spec lives behind an [`Arc`] and is shared by clone of
/// that `Arc`.
pub(crate) struct AgentRebuildSpec {
    /// Per-session return profile for temporary browser/computer mode. Keep the
    /// resolved definition (including user tool overrides), not a fresh preset.
    pub regular_mode_definition: parking_lot::Mutex<Option<AgentDefinition>>,
    pub working_directory: PathBuf,
    pub terminal_backend: Arc<dyn TerminalBackend>,
    pub fs_backend: Arc<dyn AsyncFileSystem>,
    pub tools_notification_handle: ToolNotificationHandle,
    pub bridge_state_path: PathBuf,
    pub session_env: Arc<HashMap<String, String>>,
    pub models_manager: crate::agent::models::ModelsManager,
    pub compaction_policy: CompactionPolicy,
    pub reminder_policy: ReminderPolicy,
    pub memory_enabled: bool,
    pub memory_global_path: Option<String>,
    pub memory_workspace_path: Option<String>,
    pub memory_backend: Option<Arc<dyn MemoryBackend>>,
    pub web_search_config: WebSearchConfig,
    /// `[toolset.web_search]` domain policy, resolved once at spawn and applied
    /// to both search paths (the hosted `tool_overrides` merge and the
    /// client-side `WebSearchConfig`) so they never diverge.
    pub web_search_domains: Option<echo_agent_sampling_types::WebSearchOptions>,
    pub backend_search: bool,
    pub web_fetch_config: WebFetchConfig,
    pub image_gen_config: ImageGenConfig,
    pub video_gen_config: VideoGenConfig,
    pub app_builder_deployer_config: AppBuilderDeployerConfig,
    pub media_gen_batch_limits: echo_agent_tools::media_gen_limits::MediaGenBatchLimits,
    pub write_file_enabled: bool,
    pub subagents_enabled: bool,
    pub subagent_toggle: HashMap<String, bool>,
    pub background_workflows_enabled: bool,
    pub ask_user_question_enabled: bool,
    pub persona_summaries: Vec<String>,
    pub prompt_audience: PromptAudience,
    pub role_instructions: Option<String>,
    pub persona_instructions: Option<String>,
    pub skills_config: SkillsConfig,
    /// Resolved vendor-compat config (from `Config::compat_resolved`), threaded
    /// into skills / rules / AGENTS.md discovery via the builder.
    pub compat: CompatConfig,
    pub context_window_tokens: u64,
    pub prompt_working_directory: Option<String>,
    pub lsp: Option<Arc<dyn LspBackend>>,
    pub plugin_registry: Option<Arc<echo_agent_core::plugins::PluginRegistry>>,
    pub api_key_provider: Option<SharedApiKeyProvider>,
    pub attribution_callback: Option<echo_agent_tools::SharedAttributionCallback>,
    pub tool_params_json: ResolvedToolParamsJson,
    pub subagent_event_tx: Option<UnboundedSender<SubagentEvent>>,
    pub monitor_event_buffer: Option<MonitorEventBuffer>,
    pub user_question_tx: UnboundedSender<UserQuestionRequest>,
    pub subagent_depth: u32,
    pub subagents_max_depth: u32,
    pub session_id_str: String,
    pub blocking_wait_depth: Arc<crate::tools::tool_context::BlockingWaitState>,
    pub respect_gitignore: bool,
    pub path_not_found_hints: bool,
    /// Fire side of the scheduler mode. The spawn copies the same resolution
    /// onto [`SessionHandle::scheduler_background_loops`](crate::session::SessionHandle),
    /// which is what clients read — keep the two on one resolve.
    pub scheduler_background_loops: bool,
    pub mcp_state: Arc<tokio::sync::Mutex<crate::session::mcp_servers::McpState>>,
    pub managed_gateway_tool_client:
        Option<echo_agent_tools::types::resources::ManagedGatewayToolClient>,
    pub is_non_interactive: bool,
    pub system_prompt_label: String,
    pub owner_session_id: Option<String>,
    pub parent_scheduler_handle: Option<
        echo_agent_tools::implementations::echo_agent_build::scheduler::types::SchedulerHandle,
    >,
}
impl AgentRebuildSpec {
    /// Build a fresh [`Agent`] from this spec and an [`AgentDefinition`].
    ///
    /// This is the canonical construction path; see module docs for the
    /// invariant. The destructure pattern below is intentional —
    /// `#[deny(unused_variables)]` ensures any newly added spec field is
    /// used here, otherwise compilation fails.
    #[deny(unused_variables)]
    pub(crate) async fn build_agent(
        self: &Arc<Self>,
        definition: AgentDefinition,
    ) -> Result<Agent, AgentBuildError> {
        let (agent, _build_elapsed) = self.build_agent_inner(definition, None, None).await?;
        Ok(agent)
    }
    /// Build an agent with optional one-shot overrides for initial spawn.
    ///
    /// `persisted_skill_names`: restored into the `SkillManager` before
    /// `seed()` to prevent duplicate system-reminder injection on resume.
    ///
    /// `preloaded_skills`: parent-discovered skills passed to
    /// `AgentBuilder::with_preloaded_skills()` to bypass filesystem
    /// discovery in subagents.
    ///
    /// Both are consumed once — the rebuild path (`build_agent`) passes
    /// `None` for both so zero-turn model switches get fresh discovery.
    /// Returns the built agent and the pure construction time (entry to
    /// `SB_BUILDER_DONE`, before the batched resource seed), so the caller can
    /// attribute `AgentBuild` and `ToolSetup` phases to the same boundaries the
    /// waterfall marks use.
    pub(crate) async fn build_agent_with_initial_overrides(
        self: &Arc<Self>,
        definition: AgentDefinition,
        persisted_skill_names: Option<std::collections::HashSet<String>>,
        preloaded_skills: Option<Vec<echo_agent_tools::implementations::skills::types::SkillInfo>>,
    ) -> Result<(Agent, std::time::Duration), AgentBuildError> {
        self.build_agent_inner(definition, persisted_skill_names, preloaded_skills)
            .await
    }
    #[deny(unused_variables)]
    async fn build_agent_inner(
        self: &Arc<Self>,
        definition: AgentDefinition,
        persisted_skill_names: Option<std::collections::HashSet<String>>,
        preloaded_skills: Option<Vec<echo_agent_tools::implementations::skills::types::SkillInfo>>,
    ) -> Result<(Agent, std::time::Duration), AgentBuildError> {
        let build_phase_start = std::time::Instant::now();
        let Self {
            regular_mode_definition,
            working_directory,
            terminal_backend,
            fs_backend,
            tools_notification_handle,
            bridge_state_path,
            session_env,
            models_manager,
            compaction_policy,
            reminder_policy,
            memory_enabled,
            memory_global_path,
            memory_workspace_path,
            memory_backend,
            web_search_config,
            web_search_domains,
            backend_search,
            web_fetch_config,
            image_gen_config,
            video_gen_config,
            app_builder_deployer_config,
            media_gen_batch_limits: _,
            write_file_enabled,
            subagents_enabled,
            subagent_toggle,
            background_workflows_enabled,
            ask_user_question_enabled,
            persona_summaries,
            prompt_audience,
            role_instructions,
            persona_instructions,
            skills_config,
            compat,
            context_window_tokens,
            prompt_working_directory,
            lsp,
            plugin_registry,
            api_key_provider,
            attribution_callback,
            tool_params_json,
            subagent_event_tx,
            monitor_event_buffer,
            user_question_tx,
            subagent_depth,
            subagents_max_depth,
            session_id_str,
            blocking_wait_depth,
            respect_gitignore,
            path_not_found_hints,
            scheduler_background_loops,
            mcp_state,
            managed_gateway_tool_client,
            is_non_interactive,
            system_prompt_label,
            owner_session_id,
            parent_scheduler_handle,
        } = self.as_ref();
        let _ = (mcp_state, regular_mode_definition);
        #[allow(unused_variables)]
        let is_cursor_template =
            crate::session::is_cursor_system_template(&definition.system_prompt);
        let mut definition = definition;
        if let Some(cfg_opts) = web_search_domains.clone() {
            definition
                .tool_overrides
                .get_or_insert_with(Default::default)
                .web_search = Some(cfg_opts);
        }
        let session_env = {
            let mut env = session_env.as_ref().clone();
            env.insert("ECHO_AGENT_SESSION_ID".to_string(), session_id_str.clone());
            Arc::new(env)
        };
        let mut builder = AgentBuilder::new(
            working_directory.clone(),
            terminal_backend.clone(),
            tools_notification_handle.clone(),
        )
        .from_definition(definition)
        .with_compaction_policy(compaction_policy.clone())
        .with_reminder_policy(reminder_policy.clone())
        .with_memory_enabled(*memory_enabled)
        .with_memory_paths(memory_global_path.clone(), memory_workspace_path.clone())
        .with_is_non_interactive(*is_non_interactive)
        .with_system_prompt_label(system_prompt_label.clone())
        .with_session_env(session_env.clone())
        .with_state_path(bridge_state_path.clone())
        .with_web_search_config(web_search_config.clone())
        .with_backend_search(*backend_search)
        .with_image_gen_config(image_gen_config.clone())
        .with_video_gen_config(video_gen_config.clone())
        .with_app_builder_deployer_config(app_builder_deployer_config.clone())
        .with_web_fetch_config(web_fetch_config.clone())
        .with_write_file_enabled(*write_file_enabled)
        .with_fs(fs_backend.clone())
        .with_subagents_enabled(*subagents_enabled)
        .with_subagent_toggle(subagent_toggle.clone())
        .with_background_workflows_enabled(*background_workflows_enabled)
        .with_task_model_slugs(
            models_manager
                .available()
                .keys()
                .map(|model_id| model_id.0.to_string())
                .collect::<Vec<_>>(),
        )
        .with_ask_user_question_enabled(*ask_user_question_enabled)
        .with_persona_summaries(persona_summaries.clone())
        .with_prompt_audience(*prompt_audience)
        .with_role_instructions(role_instructions.clone())
        .with_persona_instructions(persona_instructions.clone())
        .with_skills_config(skills_config.clone())
        .with_compat_config(*compat)
        .with_context_window(*context_window_tokens)
        .with_mcp_max_output_bytes(
            crate::util::config::resolve_max_mcp_output_bytes_for_cwd(working_directory),
        );
        if let Some(owner_session_id) = owner_session_id.clone() {
            builder = builder.with_owner_session_id(owner_session_id);
        }
        if let Some(handle) = parent_scheduler_handle.clone() {
            builder = builder.with_parent_scheduler_handle(handle);
        }
        if let Some(memory_backend) = memory_backend.clone() {
            builder = builder.with_memory_backend(memory_backend);
        }
        if let Some(lsp) = lsp.clone() {
            builder = builder.with_lsp(lsp);
        }
        if let Some(plugin_registry) = plugin_registry.clone() {
            builder = builder.with_plugin_registry(plugin_registry);
        }
        if let Some(api_key_provider) = api_key_provider.clone() {
            builder = builder.with_api_key_provider(api_key_provider);
        }
        if let Some(attribution_callback) = attribution_callback.clone() {
            builder = builder.with_attribution_callback(attribution_callback);
        }
        if let Some(bash_params_json) = tool_params_json.bash.clone() {
            builder = builder.with_bash_params(bash_params_json);
        }
        if let Some(ask_user_question_params_json) = tool_params_json.ask_user_question.clone() {
            builder = builder.with_ask_user_question_params(ask_user_question_params_json);
        }
        if let Some(prompt_working_directory) = prompt_working_directory.clone() {
            builder = builder.with_prompt_working_directory(prompt_working_directory);
        }
        if let Some(names) = persisted_skill_names {
            builder = builder.with_persisted_announced_skill_names(names);
        }
        if let Some(skills) = preloaded_skills {
            builder = builder.with_preloaded_skills(skills);
        }
        let agent = builder.build().await?;
        crate::waterfall::mark(session_id_str, crate::waterfall::stage::SB_BUILDER_DONE);
        let agent_build_elapsed = build_phase_start.elapsed();
        let model_validator = models_manager.clone();
        agent
            .tool_bridge()
            .update_resources_with(|resources| {
                resources
                    .insert(
                        TaskModelValidator::new(move |requested| {
                            model_validator.task_model_error(requested)
                        }),
                    );
                if let Some(event_tx) = subagent_event_tx.clone() {
                    use echo_agent_tools::implementations::echo_agent_build::task::backend::{
                        ChannelBackend, SubagentBackendResource,
                    };
                    use echo_agent_tools::implementations::echo_agent_build::task::types::{
                        MaxSubagentDepth, SessionIdResource, SubagentDepthCounter,
                        SubagentEventSender,
                    };
                    resources
                        .insert(
                            SubagentBackendResource(
                                Arc::new(
                                    ChannelBackend::for_session(
                                        event_tx.clone(),
                                        session_id_str.clone(),
                                    ),
                                ),
                            ),
                        );
                    resources.insert(SubagentDepthCounter(*subagent_depth));
                    resources.insert(MaxSubagentDepth(*subagents_max_depth));
                    resources.insert(SessionIdResource(session_id_str.clone()));
                    resources.insert(SubagentEventSender(event_tx));
                    resources
                        .insert(
                            crate::tools::tool_context::subagent_foreground_wait(
                                Arc::clone(blocking_wait_depth),
                            ),
                        );
                    if let Some(buffer) = monitor_event_buffer.clone() {
                        resources.insert(buffer);
                    }
                }
                resources
                    .insert(
                        echo_agent_tools::types::resources::RespectGitignore(
                            *respect_gitignore,
                        ),
                    );
                resources
                    .insert(
                        echo_agent_tools::types::resources::SchedulerBackgroundLoops(
                            *scheduler_background_loops,
                        ),
                    );
                resources
                    .insert(
                        echo_agent_tools::types::resources::PathNotFoundHints(
                            *path_not_found_hints,
                        ),
                    );
                if let Some(client) = managed_gateway_tool_client.clone() {
                    resources.insert(client);
                }
                {
                    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::UserQuestionSender;
                    resources.insert(UserQuestionSender(user_question_tx.clone()));
                }
            })
            .await;
        Ok((agent, agent_build_elapsed))
    }
}
/// Build a stub [`AgentRebuildSpec`] for unit tests.
///
/// Every field is set to a minimal default suitable for test `SessionActor`
/// literals and focused `build_agent` tests.
#[cfg(test)]
pub(crate) fn test_rebuild_spec_default() -> Arc<AgentRebuildSpec> {
    let (uq_tx, _uq_rx) = tokio::sync::mpsc::unbounded_channel();
    Arc::new(AgentRebuildSpec {
        regular_mode_definition: parking_lot::Mutex::new(None),
        working_directory: std::env::temp_dir(),
        terminal_backend: Arc::new(
            echo_agent_tools::computer::local::LocalTerminalBackend::new_local(
                echo_agent_tools::computer::local::SearchShadowConfig::default(),
            ),
        ),
        fs_backend: Arc::new(echo_agent_tools::computer::local::LocalFs),
        tools_notification_handle: ToolNotificationHandle::noop(),
        bridge_state_path: std::env::temp_dir().join("test_tool_state.json"),
        session_env: Arc::new(HashMap::new()),
        models_manager: crate::agent::models::ModelsManager::default(),
        compaction_policy: CompactionPolicy::default(),
        reminder_policy: ReminderPolicy::default(),
        memory_enabled: false,
        memory_global_path: None,
        memory_workspace_path: None,
        memory_backend: None,
        web_search_config: WebSearchConfig::default(),
        web_search_domains: None,
        backend_search: false,
        web_fetch_config: WebFetchConfig::Disabled,
        image_gen_config: ImageGenConfig::default(),
        video_gen_config: VideoGenConfig::default(),
        app_builder_deployer_config: AppBuilderDeployerConfig::default(),
        media_gen_batch_limits: echo_agent_tools::media_gen_limits::MediaGenBatchLimits::default(),
        write_file_enabled: true,
        subagents_enabled: false,
        subagent_toggle: HashMap::new(),
        background_workflows_enabled: false,
        ask_user_question_enabled: true,
        persona_summaries: vec![],
        prompt_audience: PromptAudience::Primary,
        role_instructions: None,
        persona_instructions: None,
        skills_config: SkillsConfig::default(),
        compat: CompatConfig::default(),
        context_window_tokens: 256_000,
        prompt_working_directory: None,
        lsp: None,
        plugin_registry: None,
        api_key_provider: None,
        attribution_callback: None,
        tool_params_json: ResolvedToolParamsJson::default(),
        subagent_event_tx: None,
        monitor_event_buffer: None,
        user_question_tx: uq_tx,
        subagent_depth: 0,
        subagents_max_depth:
            echo_agent_tools::implementations::echo_agent_build::task::MAX_SUBAGENT_DEPTH,
        session_id_str: "test-session".to_string(),
        blocking_wait_depth: Arc::new(crate::tools::tool_context::BlockingWaitState::new()),
        respect_gitignore: false,
        scheduler_background_loops: true,
        path_not_found_hints: false,
        mcp_state: Arc::new(tokio::sync::Mutex::new(
            crate::session::mcp_servers::McpState::new(vec![]),
        )),
        managed_gateway_tool_client: None,
        is_non_interactive: false,
        system_prompt_label: echo_agent_core::DEFAULT_SYSTEM_PROMPT_LABEL.to_string(),
        owner_session_id: Some("test-session".to_string()),
        parent_scheduler_handle: None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    fn model_entry(internal_id: &str) -> ModelEntry {
        ModelEntry::fallback(internal_id, &EndpointsConfig::default())
    }
    fn task_description(agent: &Agent) -> String {
        let toolset = agent.tool_bridge().toolset();
        let task_name = toolset
            .tool_name_for_kind(echo_agent_tools::types::tool::ToolKind::Task)
            .expect("EchoAgentBuild Task tool should be present");
        toolset
            .tool_definitions()
            .into_iter()
            .find(|definition| definition.function.name == task_name)
            .and_then(|definition| definition.function.description)
            .expect("EchoAgentBuild Task description should be present")
    }
    /// The `[toolset.web_search]` policy is authoritative on the backend-hosted
    /// path: agent frontmatter is model-writable (`.echo-agent/agents/*.md`), so a
    /// configured blocklist must survive a frontmatter allowlist, matching the
    /// client-side `resolve_filters`. With no configured policy, frontmatter
    /// still applies.
    #[tokio::test(flavor = "current_thread")]
    async fn config_web_search_domains_beat_agent_frontmatter() {
        use echo_agent_sampling_types::{HostedTool, ToolOverrides, WebSearchOptions};
        let frontmatter = WebSearchOptions {
            allowed_domains: Some(vec!["attacker.example".into()]),
            excluded_domains: None,
        };
        let configured = WebSearchOptions {
            allowed_domains: None,
            excluded_domains: Some(vec!["reddit.com".into()]),
        };
        let definition = || {
            let mut d = AgentDefinition::default_echo_agent_build();
            d.tool_overrides = Some(ToolOverrides {
                x_search: None,
                web_search: Some(frontmatter.clone()),
            });
            d
        };
        let spec_with = |domains: Option<WebSearchOptions>| {
            let mut spec = test_rebuild_spec_default();
            let spec_mut =
                Arc::get_mut(&mut spec).expect("test rebuild spec should be uniquely owned");
            spec_mut.web_search_domains = domains;
            spec_mut.backend_search = true;
            spec_mut.web_search_config = WebSearchConfig::Enabled {
                api_key: "test-key".to_string(),
                base_url: "https://api.echo-agent.invalid/v1".to_string(),
                model: "echo-agent-4".to_string(),
                extra_headers: Default::default(),
                alpha_test_key: None,
                allowed_domains: None,
                excluded_domains: None,
            };
            spec
        };
        tokio::task::LocalSet::new()
            .run_until(async {
                let agent = spec_with(Some(configured.clone()))
                    .build_agent(definition())
                    .await
                    .expect("agent build should succeed");
                assert_eq!(
                    agent
                        .definition()
                        .tool_overrides
                        .as_ref()
                        .and_then(|o| o.web_search.clone()),
                    Some(configured.clone()),
                    "config must replace the frontmatter allowlist"
                );
                assert!(
                    agent.hosted_tools().contains(&HostedTool::WebSearch {
                        options: Some(configured.clone()),
                    }),
                    "the hosted tool carries the configured policy: {:?}",
                    agent.hosted_tools()
                );
                let agent = spec_with(None)
                    .build_agent(definition())
                    .await
                    .expect("agent build should succeed");
                assert_eq!(
                    agent
                        .definition()
                        .tool_overrides
                        .as_ref()
                        .and_then(|o| o.web_search.clone()),
                    Some(frontmatter.clone())
                );
            })
            .await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn rebuild_projects_fresh_public_model_keys_into_task_description() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut spec = test_rebuild_spec_default();
                Arc::get_mut(&mut spec)
                    .expect("test rebuild spec should be uniquely owned")
                    .subagents_enabled = true;
                let models_manager = spec.models_manager.clone();
                models_manager
                    .insert_test_entry("zeta-public", model_entry("internal-zeta"));
                models_manager
                    .insert_test_entry("alpha-public", model_entry("internal-alpha"));
                let mut hidden = model_entry("internal-hidden");
                hidden.info.hidden = true;
                models_manager.insert_test_entry("private-hidden-model", hidden);
                let mut unselectable = model_entry("internal-unselectable");
                unselectable.info.user_selectable = false;
                models_manager
                    .insert_test_entry("private-unselectable-model", unselectable);
                let first = spec
                    .build_agent(AgentDefinition::default_echo_agent_build())
                    .await
                    .expect("first agent build should succeed");
                let first_description = task_description(&first);
                assert!(
                    first_description.contains(
                        "If the user explicitly asks for the model of a subagent/task, you may ONLY use model slugs from this list:\n\
                         - alpha-public\n\
                         - zeta-public"
                    )
                );
                assert!(!first_description.contains("private-hidden-model"));
                assert!(!first_description.contains("private-unselectable-model"));
                assert!(!first_description.contains("internal-alpha"));
                let validator = first
                    .tool_bridge()
                    .toolset()
                    .get_resource_cloned::<TaskModelValidator>()
                    .await
                    .expect("Task model validator should be registered");
                assert!(validator.error_for("alpha-public").is_none());
                assert!(validator.error_for("private-hidden-model").is_some());
                models_manager
                    .insert_test_entry("beta-public", model_entry("internal-beta"));
                assert!(validator.error_for("beta-public").is_none());
                let rebuilt = spec
                    .build_agent(AgentDefinition::default_echo_agent_build())
                    .await
                    .expect("rebuilt agent should succeed");
                let rebuilt_description = task_description(&rebuilt);
                assert!(
                    rebuilt_description.contains(
                        "If the user explicitly asks for the model of a subagent/task, you may ONLY use model slugs from this list:\n\
                         - alpha-public\n\
                         - beta-public\n\
                         - zeta-public"
                    )
                );
            })
            .await;
    }
}
