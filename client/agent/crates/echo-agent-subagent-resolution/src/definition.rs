//! Production subagent definition discovery and tool-policy resolution.
use crate::config::{SubagentPersona, SubagentRole};
use crate::types::{EffectiveRuntimeConfig, ResolutionError};
use echo_agent_core::config::{AgentDefinition, IsolationMode};
use echo_agent_core::plugins::PluginRegistry;
use echo_agent_core::prompt::context::{PromptAudience, PromptContext};
use echo_agent_tool_types::{SubagentCapabilityMode, SubagentIsolationMode};
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentCapabilityModeExt, SubagentRuntimeOverrides, prune_orphaned_background_task_tools,
};
use echo_agent_tools::registry::types::ToolConfig;
use echo_agent_tools::types::compat::CompatConfig;
use echo_agent_tools::types::template_renderer::TemplateRenderer;
use echo_agent_tools::types::tool::ToolKind;
use std::collections::HashMap;
use std::path::Path;
/// Inputs that affect definition discovery and spawn permission.
pub struct DefinitionResolutionContext<'a> {
    pub cwd: &'a Path,
    pub plugins: Option<&'a PluginRegistry>,
    pub cli_agents: &'a [AgentDefinition],
    pub toggles: &'a HashMap<String, bool>,
    pub allowed_types: Option<&'a [String]>,
}
/// Inputs for validating a type when only session CLI names are available.
pub struct DefinitionValidationContext<'a> {
    pub cwd: &'a Path,
    pub plugins: Option<&'a PluginRegistry>,
    pub cli_agent_names: &'a [String],
    pub toggles: &'a HashMap<String, bool>,
    pub allowed_types: Option<&'a [String]>,
}
/// Parent/runtime inputs that choose the production child harness flavor.
pub struct HarnessToolsetContext<'a> {
    pub harness_override: Option<&'a str>,
    pub parent_agent_name: Option<&'a str>,
    pub parent_model_agent_type: Option<&'a str>,
    pub file_tool_overrides: Option<&'a [ToolConfig]>,
}
/// `false` twin: the alternate flavors re-select toolset presets and
/// templates, so none is representable when the optional harness is compiled
/// out. Keeps ungated call sites compiling.
pub fn subagent_harness_flavor_is_representable(_agent_type: &str) -> bool {
    false
}
/// Apply the production parent/harness-dependent child toolset selection.
pub fn apply_harness_toolset(
    #[allow(unused_variables)] subagent_type: &str,
    context: &HarnessToolsetContext<'_>,
    definition: &mut AgentDefinition,
) {
    let flavor_agent = context.harness_override.or_else(|| {
        context
            .parent_agent_name
            .filter(|name| subagent_harness_flavor_is_representable(name))
            .or(context.parent_model_agent_type)
    });
    if flavor_agent.is_some_and(subagent_harness_flavor_is_representable) {
    } else if let Some(file_tools) = context.file_tool_overrides {
        definition.override_file_tools(file_tools.to_vec());
    }
}
/// Discover the same project/builtin/user/plugin definition used by production,
/// with session CLI definitions as the final fallback.
pub fn discover_agent_definition(
    subagent_type: &str,
    context: &DefinitionResolutionContext<'_>,
) -> Option<AgentDefinition> {
    echo_agent_core::discovery::by_name_in_cwd_with_plugins(
        subagent_type,
        context.cwd,
        context.plugins,
    )
    .or_else(|| {
        context
            .cli_agents
            .iter()
            .find(|definition| definition.name == subagent_type)
            .cloned()
    })
}
/// Sorted model-facing names available under the current discovery context.
pub fn available_agent_names(context: &DefinitionResolutionContext<'_>) -> Vec<String> {
    let mut available: Vec<String> = echo_agent_core::discovery::all_subagents_with_plugins(
        context.cwd,
        context.toggles,
        context.plugins,
    )
    .into_iter()
    .map(|entry| entry.name)
    .collect();
    for definition in context.cli_agents {
        if context
            .toggles
            .get(&definition.name)
            .copied()
            .unwrap_or(true)
            && !available.contains(&definition.name)
        {
            available.push(definition.name.clone());
        }
    }
    available.sort();
    available
}
/// Apply the production toggle and parent allow-list gates.
pub fn gate_agent_definition(
    subagent_type: &str,
    context: &DefinitionResolutionContext<'_>,
) -> Result<(), ResolutionError> {
    if !context.toggles.get(subagent_type).copied().unwrap_or(true) {
        return Err(ResolutionError::Disabled {
            subagent_type: subagent_type.to_string(),
        });
    }
    if let Some(allowed) = context.allowed_types
        && !allowed
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(subagent_type))
    {
        return Err(ResolutionError::NotAllowed {
            subagent_type: subagent_type.to_string(),
            allowed: allowed.to_vec(),
        });
    }
    Ok(())
}
/// Validate discovery, toggle, and allow-list gates without cloning definitions.
pub fn validate_agent_name(
    subagent_type: &str,
    context: &DefinitionValidationContext<'_>,
) -> Result<(), ResolutionError> {
    let resolves = context
        .cli_agent_names
        .iter()
        .any(|name| name == subagent_type)
        || echo_agent_core::discovery::by_name_in_cwd_with_plugins(
            subagent_type,
            context.cwd,
            context.plugins,
        )
        .is_some();
    if !resolves {
        let mut available: Vec<String> = echo_agent_core::discovery::all_subagents_with_plugins(
            context.cwd,
            context.toggles,
            context.plugins,
        )
        .into_iter()
        .map(|entry| entry.name)
        .collect();
        for name in context.cli_agent_names {
            if context.toggles.get(name).copied().unwrap_or(true) && !available.contains(name) {
                available.push(name.clone());
            }
        }
        available.sort();
        return Err(ResolutionError::Unknown {
            subagent_type: subagent_type.to_owned(),
            available,
        });
    }
    let gate_context = DefinitionResolutionContext {
        cwd: context.cwd,
        plugins: context.plugins,
        cli_agents: &[],
        toggles: context.toggles,
        allowed_types: context.allowed_types,
    };
    gate_agent_definition(subagent_type, &gate_context)
}
/// Discover and gate one production agent definition.
pub fn resolve_agent_definition(
    subagent_type: &str,
    context: &DefinitionResolutionContext<'_>,
) -> Result<AgentDefinition, ResolutionError> {
    let definition = discover_agent_definition(subagent_type, context).ok_or_else(|| {
        ResolutionError::Unknown {
            subagent_type: subagent_type.to_string(),
            available: available_agent_names(context),
        }
    })?;
    gate_agent_definition(subagent_type, context)?;
    Ok(definition)
}
/// Resolve the role selected by production: type-specific first, then persona.
pub fn select_role<'a>(
    subagent_type: &str,
    overrides: &SubagentRuntimeOverrides,
    roles: &'a HashMap<String, SubagentRole>,
) -> (Option<&'a SubagentRole>, Option<String>) {
    if let Some(role) = roles.get(subagent_type) {
        return (Some(role), Some(subagent_type.to_string()));
    }
    let Some(persona) = overrides.persona.as_deref() else {
        return (None, None);
    };
    match roles.get(persona) {
        Some(role) => (Some(role), Some(persona.to_string())),
        None => (None, None),
    }
}
/// Fill runtime values whose defaults live on the resolved agent definition.
pub fn apply_definition_runtime_defaults(
    runtime: &mut EffectiveRuntimeConfig,
    definition: &AgentDefinition,
) {
    if runtime.capability_mode.is_none() {
        runtime.capability_mode = definition.capability_mode;
    }
    if runtime.reasoning_effort.is_none() {
        runtime.reasoning_effort = definition
            .effort
            .map(|effort| <&str>::from(effort).to_string());
    }
    if runtime.isolation == SubagentIsolationMode::None
        && definition.isolation == Some(IsolationMode::Worktree)
    {
        runtime.isolation = SubagentIsolationMode::Worktree;
    }
}
/// Apply capability filtering and recursion depth to the exact production
/// definition toolset.
pub fn apply_child_tool_policy(
    definition: &mut AgentDefinition,
    capability_mode: Option<SubagentCapabilityMode>,
    allow_nested_subagents: bool,
) {
    if let Some(mode) = capability_mode {
        mode.filter_tool_config(&mut definition.tool_config);
    }
    if !allow_nested_subagents {
        definition
            .tool_config
            .tools
            .retain(|tool| tool.kind != Some(ToolKind::Task));
        prune_orphaned_background_task_tools(&mut definition.tool_config);
    }
}
/// Resolve runtime overrides and definition defaults in the production order.
pub fn resolve_runtime_config(
    subagent_type: &str,
    overrides: &SubagentRuntimeOverrides,
    roles: &HashMap<String, SubagentRole>,
    personas: &HashMap<String, SubagentPersona>,
    cwd: Option<&Path>,
    definition: &AgentDefinition,
) -> EffectiveRuntimeConfig {
    let (role, role_name) = select_role(subagent_type, overrides, roles);
    let mut runtime = crate::resolve_effective_overrides(overrides, role, personas, cwd, role_name);
    apply_definition_runtime_defaults(&mut runtime, definition);
    runtime
}
/// Render the same full subagent base template + definition body used by the
/// production `AgentBuilder`, for runtimes that expose only finalized tool
/// names rather than a complete `ToolBridge`.
pub fn render_subagent_system_prompt(
    definition: &AgentDefinition,
    runtime: &EffectiveRuntimeConfig,
    renderer: &TemplateRenderer,
    working_directory: &Path,
) -> Option<String> {
    let context = PromptContext {
        prompt_mode: definition.prompt_mode.clone(),
        audience: PromptAudience::Subagent,
        prompt_body: definition.prompt_body.clone(),
        include_browser_verification: definition.include_browser_verification(),
        system_prompt: definition.system_prompt.clone(),
        role_instructions: runtime.role_prompt.clone(),
        persona_instructions: runtime.persona_instructions.clone(),
        os_name: Some(format!(
            "{} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )),
        shell_path: std::env::var("SHELL").ok(),
        working_directory: Some(working_directory.to_string_lossy().into_owned()),
        current_date: Some(chrono::Local::now().format("%Y-%m-%d").to_string()),
        is_non_interactive: true,
        ..Default::default()
    };
    context.render_with_renderer(renderer)
}
/// Render project instructions as the child's prepended user message.
pub async fn render_subagent_initial_user_message(
    definition: &AgentDefinition,
    working_directory: &Path,
    compat: CompatConfig,
) -> Option<String> {
    if !definition.agents_md {
        return None;
    }
    let agents_md_files = echo_agent_core::prompt::agents_md::read_agents_config_with_paths(
        &working_directory.to_string_lossy(),
        compat,
    )
    .await;
    PromptContext {
        audience: PromptAudience::Subagent,
        system_prompt: definition.system_prompt.clone(),
        agents_md_files,
        ..Default::default()
    }
    .agents_md_user_reminder()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn context<'a>(
        cwd: &'a Path,
        toggles: &'a HashMap<String, bool>,
    ) -> DefinitionResolutionContext<'a> {
        DefinitionResolutionContext {
            cwd,
            plugins: None,
            cli_agents: &[],
            toggles,
            allowed_types: None,
        }
    }
    #[test]
    fn builtin_explore_uses_production_read_only_toolset() {
        let cwd = tempfile::tempdir().unwrap();
        let toggles = HashMap::new();
        let mut definition =
            resolve_agent_definition("explore", &context(cwd.path(), &toggles)).unwrap();
        apply_child_tool_policy(&mut definition, None, false);
        let kinds: Vec<Option<ToolKind>> = definition
            .tool_config
            .tools
            .iter()
            .map(|tool| tool.kind)
            .collect();
        assert!(kinds.contains(&Some(ToolKind::Read)));
        assert!(kinds.contains(&Some(ToolKind::Search)));
        assert!(!kinds.contains(&Some(ToolKind::Execute)));
        assert!(!kinds.contains(&Some(ToolKind::Task)));
    }
    #[test]
    fn gates_disabled_and_not_allowed_definitions() {
        let cwd = tempfile::tempdir().unwrap();
        let toggles = HashMap::from([("explore".to_string(), false)]);
        let disabled = context(cwd.path(), &toggles);
        assert!(matches!(
            resolve_agent_definition("explore", &disabled),
            Err(ResolutionError::Disabled { .. })
        ));
        let allowed = ["plan".to_string()];
        let toggles = HashMap::new();
        let restricted = DefinitionResolutionContext {
            allowed_types: Some(&allowed),
            ..context(cwd.path(), &toggles)
        };
        assert!(matches!(
            resolve_agent_definition("explore", &restricted),
            Err(ResolutionError::NotAllowed { .. })
        ));
    }
    #[test]
    fn definition_defaults_fill_runtime_without_overwriting_explicit_values() {
        let cwd = tempfile::tempdir().unwrap();
        let toggles = HashMap::new();
        let mut definition =
            resolve_agent_definition("explore", &context(cwd.path(), &toggles)).unwrap();
        definition.isolation = Some(IsolationMode::Worktree);
        let mut runtime = EffectiveRuntimeConfig::default();
        apply_definition_runtime_defaults(&mut runtime, &definition);
        assert_eq!(runtime.isolation, SubagentIsolationMode::Worktree);
    }
    #[test]
    fn full_prompt_uses_production_subagent_template_and_body() {
        let cwd = tempfile::tempdir().unwrap();
        let toggles = HashMap::new();
        let definition =
            resolve_agent_definition("explore", &context(cwd.path(), &toggles)).unwrap();
        let renderer = TemplateRenderer::new(
            HashMap::from([
                (ToolKind::Read, "read_x".to_string()),
                (ToolKind::List, "list_x".to_string()),
                (ToolKind::Search, "search_x".to_string()),
            ]),
            HashMap::new(),
        );
        let prompt = render_subagent_system_prompt(
            &definition,
            &EffectiveRuntimeConfig::default(),
            &renderer,
            cwd.path(),
        )
        .unwrap();
        assert!(prompt.contains("<project_instructions_spec>"));
        assert!(prompt.contains("read-only codebase exploration agent"));
        assert!(prompt.contains(&format!("Workspace Path: {}", cwd.path().display())));
        assert!(!prompt.contains("${{"));
    }
    #[tokio::test]
    async fn initial_user_message_contains_project_instructions() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::write(cwd.path().join("AGENTS.md"), "Use the project contract.").unwrap();
        let toggles = HashMap::new();
        let definition =
            resolve_agent_definition("explore", &context(cwd.path(), &toggles)).unwrap();
        let message =
            render_subagent_initial_user_message(&definition, cwd.path(), CompatConfig::default())
                .await
                .unwrap();
        assert!(message.contains("Use the project contract."));
    }
}
