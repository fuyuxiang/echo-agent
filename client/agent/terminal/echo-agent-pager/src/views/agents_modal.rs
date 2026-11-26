//! Agents modal popup — lists all agent definitions (built-in, user, project, bundled).
//!
//! Opened by `/config-agents` (alias `/agents`). Uses the shared
//! [`ModalWindow`](super::modal_window) chrome. Blocks all input until
//! closed with `Esc`.
use crate::app::bundle::{BundleState, PersonaDetail};
use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::theme::Theme;
use crate::views::modal_window::{
    self, ModalContentArea, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use echo_agent_core::config::{AgentDefinition, AgentScope, BuiltinAgentName};
use echo_agent_runtime::agent::config::AgentSelectionConfig;
use echo_agent_tools::implementations::skills::discovery::extract_first_paragraph;
use echo_agent_tools::registry::types::ToolServerConfig;
use echo_agent_tools::types::template_renderer::TemplateRenderer;
use echo_agent_tools::types::tool::ToolKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthStr;
/// Which tab is active in the agents modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsTab {
    Agents,
    Personas,
}
impl AgentsTab {
    /// All tabs in display order.
    pub const ALL: &[Self] = &[Self::Agents, Self::Personas];
    /// Display label for the tab bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::Personas => "Personas",
        }
    }
    /// Next tab (wraps around).
    pub fn next(self) -> Self {
        match self {
            Self::Agents => Self::Personas,
            Self::Personas => Self::Agents,
        }
    }
    /// Previous tab (wraps around).
    pub fn prev(self) -> Self {
        match self {
            Self::Agents => Self::Personas,
            Self::Personas => Self::Agents,
        }
    }
}
/// A single entry in the agents list.
pub struct AgentListEntry {
    pub name: String,
    pub description: String,
    pub scope: AgentScope,
    pub source_path: Option<PathBuf>,
    pub enabled: bool,
    pub is_builtin: bool,
    pub expanded: bool,
    pub definition: AgentDefinition,
}
/// Kind of inline message shown in the agents modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsModalMessageKind {
    Error,
    Success,
    Info,
}
/// Inline status message (error, success, or neutral info).
#[derive(Debug, Clone)]
pub struct AgentsModalMessage {
    pub kind: AgentsModalMessageKind,
    pub text: String,
}
impl AgentsModalMessage {
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            kind: AgentsModalMessageKind::Error,
            text: text.into(),
        }
    }
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            kind: AgentsModalMessageKind::Success,
            text: text.into(),
        }
    }
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            kind: AgentsModalMessageKind::Info,
            text: text.into(),
        }
    }
}
/// Outcome of processing input on the agents modal.
pub enum AgentsModalOutcome {
    Close,
    Changed,
    Unchanged,
    /// User pressed Enter/o — open the agent's full definition in the line viewer.
    /// Contains the source path (if file-based) or in-memory markdown content.
    ViewAgent {
        /// Display title for the viewer.
        title: String,
        /// File path on disk (preferred — opens with syntax highlighting).
        source_path: Option<PathBuf>,
        /// Fallback: in-memory markdown content (for built-in agents).
        content: Option<String>,
    },
    /// Open the persona detail/edit modal.
    OpenPersonaDetail {
        name: String,
        source_path: Option<PathBuf>,
        editable: bool,
        scope_label: String,
    },
    /// Open a user/project config file in `$EDITOR` (TUI suspends until exit).
    EditInEditor {
        path: PathBuf,
        tab: AgentsTab,
    },
}
/// User-level vs project-level config files (`~/.echo-agent` vs `{cwd}/.echo-agent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigFileScope {
    #[default]
    User,
    Project,
}
impl ConfigFileScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
    pub fn toggle(self) -> Self {
        match self {
            Self::User => Self::Project,
            Self::Project => Self::User,
        }
    }
}
/// Which field is focused in a create form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateField {
    Name,
    Description,
    Instructions,
    Scope,
}
/// State for the inline create-persona form.
pub struct PersonaCreateInput {
    name: LineEditor,
    description: LineEditor,
    instructions: LineEditor,
    scope: ConfigFileScope,
    active_field: CreateField,
}
impl PersonaCreateInput {
    fn new() -> Self {
        Self {
            name: LineEditor::default(),
            description: LineEditor::default(),
            instructions: LineEditor::default(),
            scope: ConfigFileScope::User,
            active_field: CreateField::Name,
        }
    }
    pub fn name(&self) -> &str {
        self.name.text()
    }
    pub fn description(&self) -> &str {
        self.description.text()
    }
    pub fn instructions(&self) -> &str {
        self.instructions.text()
    }
    pub fn scope(&self) -> ConfigFileScope {
        self.scope
    }
    pub fn active_field(&self) -> CreateField {
        self.active_field
    }
    fn name_editor(&self) -> &LineEditor {
        &self.name
    }
    fn description_editor(&self) -> &LineEditor {
        &self.description
    }
    fn instructions_editor(&self) -> &LineEditor {
        &self.instructions
    }
    fn active_editor_mut(&mut self) -> Option<&mut LineEditor> {
        let field = self.active_field;
        self.field_editor_mut(field)
    }
    fn field_editor_mut(&mut self, field: CreateField) -> Option<&mut LineEditor> {
        match field {
            CreateField::Name => Some(&mut self.name),
            CreateField::Description => Some(&mut self.description),
            CreateField::Instructions => Some(&mut self.instructions),
            CreateField::Scope => None,
        }
    }
    #[cfg(test)]
    fn set_field_text(&mut self, field: CreateField, text: impl Into<String>) {
        if let Some(editor) = self.field_editor_mut(field) {
            editor.set_text(text);
        }
    }
    #[cfg(test)]
    fn set_field_cursor_byte(&mut self, field: CreateField, cursor_byte: usize) -> LineEditOutcome {
        self.field_editor_mut(field)
            .map_or(LineEditOutcome::Unhandled, |editor| {
                editor.set_cursor_byte(cursor_byte)
            })
    }
}
/// Pending confirmation action (delete local persona).
pub enum PersonaConfirmAction {
    Delete { name: String, path: PathBuf },
}
/// Modal state for the agents listing.
pub struct AgentsModalState {
    pub window: ModalWindowState,
    /// Currently active tab (source of truth).
    ///
    /// `window.active_tab` (a `usize` index) is derived from this in the
    /// render path via `AgentsTab::ALL.position()`. Only this field
    /// should be mutated by input handlers; the window's copy is a
    /// rendering hint synced each frame.
    pub active_tab: AgentsTab,
    pub agents: Vec<AgentListEntry>,
    pub selected: usize,
    pub scroll: usize,
    search: LineEditor,
    pub search_active: bool,
    /// Maps screen Y position to agent index. Rebuilt every render frame
    /// for mouse click → agent selection.
    pub(crate) row_map: Vec<(u16, usize)>,
    /// Content area rect from the last render (for click bounds checking).
    pub(crate) content_rect: Option<Rect>,
    pub persona_input: Option<PersonaCreateInput>,
    pub persona_confirm: Option<PersonaConfirmAction>,
    /// Inline message shown briefly. Cleared on next action.
    pub message: Option<AgentsModalMessage>,
    /// Working directory for rebuilding the agent list.
    pub cwd: PathBuf,
    /// Snapshot of bundle catalog used to merge persona lists.
    bundle: BundleState,
    /// Resolved startup agent name (same chain as shell: `[agent]`, `ECHO_AGENT_AGENT`,
    /// model `agentType`, then `echo-agent-build`).
    pub default_agent: String,
    /// Agent running in the current session (`session/info` `agentName`).
    pub active_agent: Option<String>,
    /// Model `agentType` from the pager's default/current model catalog entry,
    /// used when re-resolving after `s` toggles `[agent] name`.
    model_agent_type: Option<String>,
    /// Plugin registry snapshot for listing plugin-provided agents
    /// (`None` when no plugins are installed/enabled).
    plugin_registry: Option<echo_agent_core::plugins::PluginRegistry>,
    pub personas: Vec<PersonaDetail>,
    pub persona_selected: usize,
    pub persona_scroll: usize,
    /// Indices of expanded personas (showing description + capability tags).
    pub persona_expanded: std::collections::HashSet<usize>,
}
/// Built-in agent names that should be shown to the user.
/// Skips internal variants (EchoAgentBuildConcise, EchoAgentBuildPlan,
/// EchoAgentBuildPlanNoSubagents, EchoAgentBuildAskUser, Codex, Opencode,
/// CursorExtended, EchoAgentBuildOrchestrator).
fn user_visible_builtins() -> &'static [BuiltinAgentName] {
    &[
        BuiltinAgentName::EchoAgentBuild,
        BuiltinAgentName::GeneralPurpose,
        BuiltinAgentName::Explore,
        BuiltinAgentName::Plan,
        BuiltinAgentName::BrowserUse,
    ]
}
impl AgentsModalState {
    /// Create a new agents modal, discovering agents from `cwd` and
    /// populating personas from `bundle`.
    pub fn new(
        cwd: &Path,
        toggle: &HashMap<String, bool>,
        bundle: &BundleState,
        model_agent_type: Option<&str>,
        active_agent: Option<String>,
        plugin_registry: Option<echo_agent_core::plugins::PluginRegistry>,
    ) -> Self {
        let agents = build_agent_list(cwd, toggle, plugin_registry.as_ref());
        let personas = merge_persona_lists(bundle, cwd);
        let default_agent = resolve_default_agent_name(cwd, model_agent_type);
        Self {
            window: ModalWindowState::with_tabs(AgentsTab::ALL.len()),
            active_tab: AgentsTab::Agents,
            agents,
            selected: 0,
            scroll: 0,
            search: LineEditor::default(),
            search_active: false,
            row_map: Vec::new(),
            content_rect: None,
            persona_input: None,
            persona_confirm: None,
            message: None,
            cwd: cwd.to_path_buf(),
            bundle: bundle.clone(),
            default_agent,
            active_agent,
            model_agent_type: model_agent_type.map(str::to_owned),
            plugin_registry,
            personas,
            persona_selected: 0,
            persona_scroll: 0,
            persona_expanded: std::collections::HashSet::new(),
        }
    }
    /// Rebuild agent list from disk after a mutation.
    fn rebuild_agents(&mut self) {
        let toggle = load_agent_toggle();
        self.agents = build_agent_list(&self.cwd, &toggle, self.plugin_registry.as_ref());
        if self.selected >= self.agents.len() {
            self.selected = self.agents.len().saturating_sub(1);
        }
    }
    /// Rebuild persona list from bundle cache + local disk.
    pub fn refresh_personas(&mut self) {
        self.personas = merge_persona_lists(&self.bundle, &self.cwd);
        self.persona_expanded.clear();
        if self.persona_selected >= self.personas.len() {
            self.persona_selected = self.personas.len().saturating_sub(1);
        }
    }
    /// Reload list data after an external editor session (e.g. `$EDITOR` on `i`).
    pub fn refresh_after_editor(&mut self, tab: AgentsTab) {
        match tab {
            AgentsTab::Agents => self.rebuild_agents(),
            AgentsTab::Personas => self.refresh_personas(),
        }
    }
    pub fn search_query(&self) -> &str {
        self.search.text()
    }
    pub fn search_cursor_byte(&self) -> usize {
        self.search.cursor_byte()
    }
    fn search_editor(&self) -> &LineEditor {
        &self.search
    }
    #[cfg(test)]
    fn search_viewport(&self, width: usize) -> echo_agent_ratatui_textarea::SingleLineViewport {
        self.search.viewport(width)
    }
    #[cfg(test)]
    fn set_search_query(&mut self, query: impl Into<String>) {
        self.search.set_text(query);
    }
    #[cfg(test)]
    fn set_search_cursor_byte(&mut self, cursor_byte: usize) -> LineEditOutcome {
        self.search.set_cursor_byte(cursor_byte)
    }
    fn reset_selection_after_search_change(&mut self) {
        match self.active_tab {
            AgentsTab::Agents => {
                if let Some(&first) = self.filtered_indices().first() {
                    self.selected = first;
                }
            }
            AgentsTab::Personas => {
                if let Some(&first) = self.filtered_persona_indices().first() {
                    self.persona_selected = first;
                }
            }
        }
    }
}
/// Build the full agent list: user-visible built-ins first, then
/// file-based agents from discovery (with dedup), then plugin-provided
/// agents under qualified `plugin:agent` names.
pub fn build_agent_list(
    cwd: &Path,
    toggle: &HashMap<String, bool>,
    plugins: Option<&echo_agent_core::plugins::PluginRegistry>,
) -> Vec<AgentListEntry> {
    let mut entries = Vec::new();
    for &builtin in user_visible_builtins() {
        let def = builtin.definition();
        let name = def.name.clone();
        let enabled = toggle.get(&name).copied().unwrap_or(true);
        entries.push(AgentListEntry {
            name,
            description: def.description.clone(),
            scope: AgentScope::BuiltIn,
            source_path: None,
            enabled,
            is_builtin: true,
            expanded: false,
            definition: def,
        });
    }
    let subagent_names: Vec<String> = BuiltinAgentName::subagent_variants()
        .iter()
        .map(|b| b.definition().name)
        .collect();
    let discovered = echo_agent_core::discovery::discover(cwd);
    fn scope_priority(scope: AgentScope) -> usize {
        match scope {
            AgentScope::Project => 3,
            AgentScope::User => 2,
            AgentScope::Bundled => 1,
            AgentScope::BuiltIn => 0,
        }
    }
    for def in discovered {
        if def.scope == AgentScope::BuiltIn {
            continue;
        }
        let is_subagent_name = subagent_names.contains(&def.name);
        if is_subagent_name && def.scope != AgentScope::Project {
            continue;
        }
        if let Some(pos) = entries.iter().position(|e| e.name == def.name) {
            let existing_priority = scope_priority(entries[pos].scope);
            if scope_priority(def.scope) > existing_priority {
                let enabled = toggle.get(&def.name).copied().unwrap_or(true);
                entries[pos] = AgentListEntry {
                    name: def.name.clone(),
                    description: def.description.clone(),
                    scope: def.scope,
                    source_path: def.source_path.clone(),
                    enabled,
                    is_builtin: false,
                    expanded: false,
                    definition: def,
                };
            }
        } else {
            let enabled = toggle.get(&def.name).copied().unwrap_or(true);
            entries.push(AgentListEntry {
                name: def.name.clone(),
                description: def.description.clone(),
                scope: def.scope,
                source_path: def.source_path.clone(),
                enabled,
                is_builtin: false,
                expanded: false,
                definition: def,
            });
        }
    }
    if let Some(registry) = plugins {
        for agent in echo_agent_core::discovery::plugin_agents(registry) {
            if entries.iter().any(|e| e.name == agent.qualified_name) {
                continue;
            }
            let enabled = toggle.get(&agent.qualified_name).copied().unwrap_or(true);
            entries.push(AgentListEntry {
                name: agent.qualified_name,
                description: agent.definition.description.clone(),
                scope: agent.scope,
                source_path: agent.definition.source_path.clone(),
                enabled,
                is_builtin: false,
                expanded: false,
                definition: agent.definition,
            });
        }
    }
    entries
}
/// Base persona list from bundle status (bundled cache catalog).
fn personas_from_bundle(bundle: &BundleState) -> Vec<PersonaDetail> {
    if !bundle.persona_details.is_empty() {
        bundle.persona_details.clone()
    } else {
        bundle
            .personas
            .iter()
            .map(|name| PersonaDetail {
                name: name.clone(),
                description: None,
                has_inputs: false,
                has_outputs: false,
                source_path: None,
                scope_label: None,
            })
            .collect()
    }
}
/// Union bundled personas with local `~/.echo-agent/personas` and `{cwd}/.echo-agent/personas`.
///
/// Bundled names take precedence; local-only names are appended with scope tags.
pub fn merge_persona_lists(bundle: &BundleState, cwd: &Path) -> Vec<PersonaDetail> {
    let mut list = personas_from_bundle(bundle);
    let mut names: std::collections::HashSet<String> =
        list.iter().map(|p| p.name.clone()).collect();
    let echo_agent_home = echo_agent_config::echo_agent_home();
    let bundled_dir = echo_agent_home.join("bundled").join("personas");
    for persona in &mut list {
        if persona.source_path.is_none() {
            let path = bundled_dir.join(format!("{}.toml", persona.name));
            if path.exists() {
                persona.source_path = Some(path.display().to_string());
                if persona.scope_label.is_none() {
                    persona.scope_label = Some("bundled".to_string());
                }
            }
        }
    }
    let dirs = [
        (
            ConfigFileScope::Project,
            cwd.join(".echo-agent").join("personas"),
        ),
        (ConfigFileScope::User, echo_agent_home.join("personas")),
    ];
    for (scope, dir) in dirs {
        append_local_personas_in_dir(&dir, scope, &mut list, &mut names);
    }
    list
}
fn append_local_personas_in_dir(
    dir: &Path,
    scope: ConfigFileScope,
    list: &mut Vec<PersonaDetail>,
    names: &mut std::collections::HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut stems: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                return None;
            }
            path.file_stem()?.to_str().map(str::to_owned)
        })
        .collect();
    stems.sort();
    for name in stems {
        if names.contains(&name) {
            continue;
        }
        let path = dir.join(format!("{name}.toml"));
        if let Some(detail) = persona_detail_from_local_file(&path, &name, scope) {
            names.insert(name);
            list.push(detail);
        }
    }
}
fn persona_detail_from_local_file(
    path: &Path,
    name: &str,
    scope: ConfigFileScope,
) -> Option<PersonaDetail> {
    let content = std::fs::read_to_string(path).ok()?;
    let table: toml::Value = toml::from_str(&content).ok()?;
    let desc = table
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            table
                .get("instructions")
                .and_then(|v| v.as_str())
                .and_then(extract_first_paragraph)
        });
    let has_inputs = table
        .get("inputs")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    let has_outputs = table
        .get("outputs")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    Some(PersonaDetail {
        name: name.to_owned(),
        description: desc,
        has_inputs,
        has_outputs,
        source_path: Some(path.display().to_string()),
        scope_label: Some(scope.label().to_string()),
    })
}
/// Load the `[subagents.toggle]` map from config.toml.
pub fn load_agent_toggle() -> HashMap<String, bool> {
    let root = match echo_agent_runtime::config::load_effective_config() {
        Ok(r) => r,
        Err(_) => return HashMap::new(),
    };
    let Some(subagents) = root.get("subagents") else {
        return HashMap::new();
    };
    let Some(toggle_table) = subagents.get("toggle") else {
        return HashMap::new();
    };
    let Some(table) = toggle_table.as_table() else {
        return HashMap::new();
    };
    table
        .iter()
        .filter_map(|(k, v)| v.as_bool().map(|b| (k.to_string(), b)))
        .collect()
}
/// Sanitize a name for use as a filename: replace non-alphanumeric chars
/// (except `-` and `_`) with `-`, require at least one alphanumeric char.
pub fn sanitize_config_name(name: &str) -> Result<String, String> {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if !sanitized.chars().any(|c| c.is_alphanumeric()) {
        return Err("Name must contain at least one alphanumeric character".to_string());
    }
    Ok(sanitized)
}
fn personas_dir_for_scope(scope: ConfigFileScope, cwd: &Path) -> PathBuf {
    match scope {
        ConfigFileScope::User => echo_agent_config::echo_agent_home().join("personas"),
        ConfigFileScope::Project => cwd.join(".echo-agent").join("personas"),
    }
}
#[derive(serde::Serialize)]
struct PersonaTomlTemplate<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<&'a str>,
}
/// Create a new persona `.toml` under user or project personas directory.
pub fn create_persona_template(
    name: &str,
    description: &str,
    instructions: &str,
    scope: ConfigFileScope,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let sanitized = sanitize_config_name(name)?;
    let personas_dir = personas_dir_for_scope(scope, cwd);
    if let Err(e) = std::fs::create_dir_all(&personas_dir) {
        return Err(format!("Failed to create personas directory: {e}"));
    }
    let path = personas_dir.join(format!("{sanitized}.toml"));
    if path.exists() {
        return Err(format!("Persona '{}' already exists", sanitized));
    }
    let desc_opt = (!description.trim().is_empty()).then(|| description.trim());
    let instr_opt = (!instructions.trim().is_empty()).then(|| instructions.trim());
    let template = PersonaTomlTemplate {
        description: desc_opt,
        instructions: instr_opt,
    };
    let content =
        toml::to_string_pretty(&template).map_err(|e| format!("Failed to format persona: {e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("Failed to write persona file: {e}"))?;
    Ok(path)
}
/// True when `path` is a deletable local persona file (user or project `.echo-agent/personas`).
pub fn persona_path_is_deletable(path: &Path) -> bool {
    config_path_is_user_or_project(path, "personas")
}
/// Shared guard: canonical path under `~/.echo-agent/{subdir}` or `{cwd}/.echo-agent/{subdir}`, not bundled.
fn config_path_is_user_or_project(path: &Path, subdir: &str) -> bool {
    let Ok(canonical) = dunce::canonicalize(path) else {
        return false;
    };
    if canonical
        .components()
        .any(|c| matches!(c, std::path::Component::Normal(s) if s == "bundled"))
    {
        return false;
    }
    let echo_agent_home = echo_agent_config::echo_agent_home();
    let in_user = dunce::canonicalize(echo_agent_home.join(subdir))
        .ok()
        .is_some_and(|d| canonical.starts_with(&d));
    let project_suffix = std::path::Path::new(".echo-agent").join(subdir);
    let in_project = canonical
        .ancestors()
        .any(|a| a.ends_with(project_suffix.as_path()));
    in_user || in_project
}
/// Whether the persona can be edited on disk from the modal (local user/project only).
pub fn persona_is_editable(persona: &PersonaDetail) -> bool {
    persona_is_deletable(persona)
}
/// Whether the persona can be deleted from the modal (local user/project only).
pub fn persona_is_deletable(persona: &PersonaDetail) -> bool {
    persona
        .source_path
        .as_ref()
        .map(|p| persona_path_is_deletable(Path::new(p)))
        .unwrap_or(false)
}
/// Delete a local persona file from disk.
pub fn delete_persona_file(path: &Path) -> Result<(), String> {
    if !persona_path_is_deletable(path) {
        if dunce::canonicalize(path).ok().is_some_and(|c| {
            c.components()
                .any(|comp| matches!(comp, std::path::Component::Normal(s) if s == "bundled"))
        }) {
            return Err("Cannot delete bundled personas".to_string());
        }
        return Err("Persona file is not in a known personas directory".to_string());
    }
    std::fs::remove_file(path).map_err(|e| format!("Failed to delete persona file: {e}"))?;
    Ok(())
}
/// Load `[agent]` from effective config (merged shell + pager config layers).
fn load_agent_selection_config() -> AgentSelectionConfig {
    echo_agent_runtime::config::load_effective_config()
        .ok()
        .and_then(|root| echo_agent_runtime::agent::config::Config::new_from_toml_cfg(&root).ok())
        .map(|cfg| cfg.agent)
        .unwrap_or_default()
}
/// Explicit `[agent] name` in config.toml (not env/CLI overrides).
fn load_config_agent_name() -> Option<String> {
    load_agent_selection_config().name.filter(|s| !s.is_empty())
}
/// Resolve the agent name new sessions would start with — mirrors
/// `MvpAgent::resolve_agent_definition` in echo-agent-runtime.
pub fn resolve_default_agent_name(cwd: &Path, model_agent_type: Option<&str>) -> String {
    let agent_config = load_agent_selection_config();
    echo_agent_runtime::agent::mvp_agent::MvpAgent::resolve_agent_definition(
        cwd,
        None,
        &agent_config,
        None,
        model_agent_type,
    )
    .name
}
fn refresh_default_agent(state: &mut AgentsModalState) {
    let model_agent_type = state.model_agent_type.as_deref();
    state.default_agent = resolve_default_agent_name(&state.cwd, model_agent_type);
}
/// Set or clear the default agent via `[agent] name` in config.toml.
///
/// Pass `Some(name)` to set, `None` to clear (remove the key).
pub fn set_default_agent(name: Option<&str>) -> Result<(), String> {
    let config_path = echo_agent_config::echo_agent_home().join("config.toml");
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Some(mut doc) = crate::config_toml_edit::read_config_document_for_edit(&config_path) else {
        return Err("Could not read or parse config.toml".to_string());
    };
    if let Some(agent_name) = name {
        if !doc.contains_key("agent") {
            doc["agent"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let agent_table = doc["agent"]
            .as_table_mut()
            .ok_or("[agent] is not a table")?;
        agent_table["name"] = toml_edit::value(agent_name);
    } else if let Some(agent_table) = doc.get_mut("agent").and_then(|v| v.as_table_mut()) {
        agent_table.remove("name");
    }
    std::fs::write(&config_path, doc.to_string())
        .map_err(|e| format!("Failed to write config.toml: {e}"))?;
    Ok(())
}
/// Toggle an agent's enabled state via `[subagents.toggle]` in config.toml.
pub fn toggle_agent(name: &str, enabled: bool) -> Result<(), String> {
    let config_path = echo_agent_config::echo_agent_home().join("config.toml");
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Some(mut doc) = crate::config_toml_edit::read_config_document_for_edit(&config_path) else {
        return Err("Could not read or parse config.toml".to_string());
    };
    if !doc.contains_key("subagents") {
        doc["subagents"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let subagents = doc["subagents"]
        .as_table_mut()
        .ok_or("subagents is not a table")?;
    if !subagents.contains_key("toggle") {
        subagents["toggle"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let toggle_table = subagents["toggle"]
        .as_table_mut()
        .ok_or("subagents.toggle is not a table")?;
    toggle_table[name] = toml_edit::value(enabled);
    std::fs::write(&config_path, doc.to_string())
        .map_err(|e| format!("Failed to write config.toml: {e}"))?;
    Ok(())
}
/// Format detail lines for an expanded agent entry.
pub fn format_agent_detail(entry: &AgentListEntry) -> Vec<String> {
    let def = &entry.definition;
    let mut lines = Vec::new();
    lines.push(format!("  Model: {}", def.model));
    let mode_label = match def.prompt_mode {
        echo_agent_core::config::PromptMode::Extend => "extend",
        echo_agent_core::config::PromptMode::Full => "full",
    };
    lines.push(format!("  Prompt mode: {mode_label}"));
    let tools = &def.tool_config.tools;
    if tools.is_empty() {
        lines.push("  Tools: (none)".to_string());
    } else {
        lines.push(format!("  Tools ({}): ", tools.len()));
        for tool in tools {
            let name = tool.name_override.as_deref().unwrap_or_else(|| {
                tool.id
                    .rsplit_once(':')
                    .map_or(tool.id.as_str(), |(_, name)| name)
            });
            lines.push(format!("    \u{2022} {name}"));
        }
    }
    if !def.skills.is_empty() {
        lines.push(format!("  Skills: {}", def.skills.join(", ")));
    }
    if let Some(ref plugin) = def.plugin_name {
        lines.push(format!("  Plugin: {plugin}"));
    }
    if let Some(ref path) = entry.source_path {
        lines.push(format!("  Source: {}", path.display()));
    }
    lines.push(format!("  Scope: {}", entry.scope.label()));
    if let Some(ref body) = def.prompt_body {
        let rendered = render_prompt_body(body, &def.tool_config);
        let char_count = rendered.chars().count();
        let truncated: String = rendered.chars().take(120).collect::<String>();
        if char_count > 120 {
            lines.push(format!("  Prompt extension: {truncated}..."));
            lines.push("  (Enter to view full)".to_string());
        } else {
            lines.push(format!("  Prompt extension: {truncated}"));
        }
    } else if entry.source_path.is_some() {
        lines.push("  Prompt extension: (in file, Enter to view)".to_string());
    } else {
        lines.push("  Prompt extension: (none)".to_string());
    }
    lines
}
/// Word-wrap text to fit within `max_width` display columns.
/// Breaks at word boundaries (spaces). Words longer than `max_width`
/// are placed on their own line (not hard-broken).
fn word_wrap(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for word in text.split_whitespace() {
        let word_width = word.width();
        if current_width == 0 {
            current = word.to_string();
            current_width = word_width;
        } else if current_width + 1 + word_width <= max_width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + word_width;
        } else {
            lines.push(current);
            current = word.to_string();
            current_width = word_width;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
/// Build viewer content for a built-in agent's prompt extension.
///
/// Shows only the `prompt_body` — the custom instructions this agent adds
/// on top of the base template. Template variables like
/// `${{ tools.by_kind.read }}` are resolved to actual tool names using
/// the agent's configured toolset.
fn synthesize_agent_markdown(entry: &AgentListEntry) -> String {
    if let Some(ref body) = entry.definition.prompt_body {
        render_prompt_body(body, &entry.definition.tool_config)
    } else {
        format!(
            "*{} uses the base system prompt with no additional instructions.*\n",
            entry.name,
        )
    }
}
/// Resolve `${{ tools.by_kind.* }}` template variables in a prompt body
/// using the agent's tool config.
fn render_prompt_body(body: &str, tool_config: &ToolServerConfig) -> String {
    let mut kind_map: HashMap<ToolKind, String> = HashMap::new();
    for tool in &tool_config.tools {
        if let Some(kind) = tool.kind {
            let name = tool.name_override.clone().unwrap_or_else(|| {
                tool.id
                    .rsplit_once(':')
                    .map_or_else(|| tool.id.clone(), |(_, n)| n.to_string())
            });
            kind_map.entry(kind).or_insert(name);
        }
    }
    let renderer = TemplateRenderer::new(kind_map, HashMap::new());
    renderer.render(body).unwrap_or_else(|_| body.to_string())
}
impl AgentsModalState {
    /// Indices of agents matching the current search query.
    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.search_query().is_empty() {
            return (0..self.agents.len()).collect();
        }
        let q = self.search_query().to_lowercase();
        self.agents
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.name.to_lowercase().contains(&q) || e.description.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }
    /// Move selection to the next visible item.
    pub fn select_next(&mut self) {
        let indices = self.filtered_indices();
        if indices.is_empty() {
            return;
        }
        let cur_pos = indices.iter().position(|&i| i == self.selected);
        let next_pos = cur_pos.map(|p| (p + 1).min(indices.len() - 1)).unwrap_or(0);
        self.selected = indices[next_pos];
    }
    /// Move selection to the previous visible item.
    pub fn select_prev(&mut self) {
        let indices = self.filtered_indices();
        if indices.is_empty() {
            return;
        }
        let cur_pos = indices.iter().position(|&i| i == self.selected);
        let next_pos = cur_pos
            .map(|p| p.saturating_sub(1))
            .unwrap_or(indices.len() - 1);
        self.selected = indices[next_pos];
    }
    /// Expand the selected agent's detail view.
    pub fn expand(&mut self) {
        if let Some(entry) = self.agents.get_mut(self.selected) {
            entry.expanded = true;
        }
    }
    /// Collapse the selected agent's detail view.
    pub fn collapse(&mut self) {
        if let Some(entry) = self.agents.get_mut(self.selected) {
            entry.expanded = false;
        }
    }
    /// Indices of personas matching the current search query.
    pub fn filtered_persona_indices(&self) -> Vec<usize> {
        if self.search_query().is_empty() {
            return (0..self.personas.len()).collect();
        }
        let q = self.search_query().to_lowercase();
        self.personas
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.name.to_lowercase().contains(&q)
                    || p.description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }
    /// Move persona selection to the next visible item.
    pub fn persona_select_next(&mut self) {
        let indices = self.filtered_persona_indices();
        if indices.is_empty() {
            return;
        }
        let cur_pos = indices.iter().position(|&i| i == self.persona_selected);
        let next_pos = cur_pos.map(|p| (p + 1).min(indices.len() - 1)).unwrap_or(0);
        self.persona_selected = indices[next_pos];
    }
    /// Move persona selection to the previous visible item.
    pub fn persona_select_prev(&mut self) {
        let indices = self.filtered_persona_indices();
        if indices.is_empty() {
            return;
        }
        let cur_pos = indices.iter().position(|&i| i == self.persona_selected);
        let next_pos = cur_pos
            .map(|p| p.saturating_sub(1))
            .unwrap_or(indices.len() - 1);
        self.persona_selected = indices[next_pos];
    }
}
fn modal_sizing(compact: bool) -> ModalSizing {
    ModalSizing {
        width_pct: 0.70,
        max_width: 100,
        min_width: 44,
        v_margin: 4,
        h_pad: 2,
        v_pad: 1,
        footer_lines: 2,
    }
    .with_compact(compact)
}
fn scope_badge(scope: AgentScope, theme: &Theme) -> (String, Style) {
    let label = match scope {
        AgentScope::BuiltIn => " built-in ",
        AgentScope::Project => " project ",
        AgentScope::User => " user ",
        AgentScope::Bundled => " bundled ",
    };
    let fg = match scope {
        AgentScope::BuiltIn => theme.accent_assistant,
        AgentScope::Project => theme.accent_user,
        AgentScope::User => theme.text_secondary,
        AgentScope::Bundled => theme.gray_dim,
    };
    (label.to_string(), Style::default().fg(fg))
}
/// Render the agents modal as a centered overlay.
pub fn render_agents_modal(
    buf: &mut Buffer,
    area: Rect,
    state: &mut AgentsModalState,
    compact: bool,
    theme: &Theme,
) {
    let active_idx = AgentsTab::ALL
        .iter()
        .position(|t| *t == state.active_tab)
        .unwrap_or(0);
    state.window.active_tab = active_idx;
    let tab_labels: Vec<&str> = AgentsTab::ALL.iter().map(|t| t.label()).collect();
    let shortcuts: Vec<Shortcut<'_>> = match state.active_tab {
        AgentsTab::Agents => build_agents_tab_shortcuts(state),
        AgentsTab::Personas => build_personas_tab_shortcuts(state),
    };
    let config = ModalWindowConfig {
        title: "Agents",
        tabs: Some(&tab_labels),
        shortcuts: &shortcuts,
        sizing: modal_sizing(compact),
        fold_info: None,
    };
    let Some(content) =
        modal_window::render_modal_window(buf, area, &mut state.window, &config, theme)
    else {
        return;
    };
    let ModalContentArea {
        content: content_area,
        ..
    } = content;
    state.content_rect = Some(content_area);
    state.row_map.clear();
    match state.active_tab {
        AgentsTab::Agents => render_agents_tab(buf, &content_area, state, theme),
        AgentsTab::Personas => render_personas_tab(buf, &content_area, state, theme),
    }
}
/// Build footer shortcuts for the Agents tab.
fn build_agents_tab_shortcuts<'a>(state: &AgentsModalState) -> Vec<Shortcut<'a>> {
    let mut shortcuts = vec![
        Shortcut {
            label: "j/k nav",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "e/\u{2192} expand",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "E/\u{2190} collapse",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Enter view",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "/ search",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "t toggle",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "s default",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Tab switch tab",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Esc close",
            clickable: false,
            id: 0,
        },
    ];
    modal_window::push_vim_nav_search_hint(&mut shortcuts, state.search_active);
    shortcuts
}
/// Build footer shortcuts for the Personas tab.
fn build_personas_tab_shortcuts<'a>(state: &AgentsModalState) -> Vec<Shortcut<'a>> {
    if state.persona_input.is_some() {
        vec![
            Shortcut {
                label: "Tab switch field",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "Enter create",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "Esc cancel",
                clickable: false,
                id: 0,
            },
        ]
    } else if state.persona_confirm.is_some() {
        vec![
            Shortcut {
                label: "y confirm",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "n/Esc cancel",
                clickable: false,
                id: 0,
            },
        ]
    } else {
        let mut shortcuts = vec![
            Shortcut {
                label: "j/k nav",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "e/\u{2192} expand",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "E/\u{2190} collapse",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "Enter view",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "/ search",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "n new",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "d delete",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "Tab switch tab",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "Esc close",
                clickable: false,
                id: 0,
            },
        ];
        modal_window::push_vim_nav_search_hint(&mut shortcuts, state.search_active);
        shortcuts
    }
}
fn render_agents_search(
    buf: &mut Buffer,
    area: Rect,
    editor: &LineEditor,
    focused: bool,
    theme: &Theme,
) {
    if area.width == 0 {
        return;
    }
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell_mut((x, area.y)) {
            cell.set_char(' ');
            cell.set_style(Style::default().fg(theme.gray_dim));
        }
    }
    let prefix = "/ ";
    let prefix_width = prefix.width() as u16;
    let painted_prefix_width = prefix_width.min(area.width);
    buf.set_span(
        area.x,
        area.y,
        &ratatui::text::Span::styled(prefix, Style::default().fg(theme.accent_user)),
        painted_prefix_width,
    );
    let editor_x = area.x + painted_prefix_width;
    let editor_width = area.width - painted_prefix_width;
    let viewport = editor.viewport(editor_width as usize);
    let leading;
    let visible: &str = if focused {
        &editor.text()[viewport.visible_byte_range.clone()]
    } else {
        leading = crate::render::line_utils::truncate_str(editor.text(), editor_width as usize);
        &leading
    };
    if editor_width > 0 {
        buf.set_string(
            editor_x,
            area.y,
            visible,
            Style::default().fg(theme.accent_user),
        );
    }
    if focused {
        let cursor_offset = painted_prefix_width
            .saturating_add(viewport.cursor_display_column as u16)
            .min(area.width - 1);
        if let Some(cell) = buf.cell_mut((area.x + cursor_offset, area.y)) {
            cell.set_style(Style::default().fg(theme.bg_base).bg(theme.text_primary));
        }
    }
}
/// Render the Agents tab content (existing agents list).
fn render_agents_tab(
    buf: &mut Buffer,
    content_area: &Rect,
    state: &mut AgentsModalState,
    theme: &Theme,
) {
    let mut y = content_area.y;
    let w = content_area.width as usize;
    if let Some(ref msg) = state.message {
        y = render_modal_message_line(buf, content_area.x, y, w, msg, theme);
    }
    if state.search_active || !state.search_query().is_empty() {
        render_agents_search(
            buf,
            Rect::new(content_area.x, y, content_area.width, 1),
            state.search_editor(),
            state.search_active,
            theme,
        );
        y += 1;
        y += 1;
    }
    let visible_height = content_area.height.saturating_sub(y - content_area.y) as usize;
    if visible_height == 0 {
        return;
    }
    let filtered = state.filtered_indices();
    if filtered.is_empty() {
        let msg = if state.search_query().is_empty() {
            "No agents found"
        } else {
            "No matching agents"
        };
        buf.set_string(content_area.x, y, msg, Style::default().fg(theme.gray_dim));
        return;
    }
    let visible_width = content_area.width as usize;
    let mut rows: Vec<FlatRow> = Vec::new();
    let mut current_group: Option<AgentGroup> = None;
    for &idx in &filtered {
        let entry = &state.agents[idx];
        let group = AgentGroup::of(entry);
        if current_group != Some(group) {
            current_group = Some(group);
            rows.push(FlatRow::GroupHeader(group));
        }
        rows.push(FlatRow::Agent(idx));
        if !entry.description.is_empty() {
            let indent = 6usize;
            let desc_w = visible_width.saturating_sub(indent);
            if desc_w > 0 {
                for line in word_wrap(&entry.description, desc_w) {
                    rows.push(FlatRow::Description(idx, line));
                }
            }
        }
        if entry.expanded {
            let details = format_agent_detail(entry);
            for line in details {
                rows.push(FlatRow::Detail(line));
            }
        }
    }
    let selected_row = rows
        .iter()
        .position(|r| matches!(r, FlatRow::Agent(i) if *i == state.selected))
        .unwrap_or(0);
    let mut selected_end = selected_row + 1;
    while selected_end < rows.len()
        && matches!(
            rows[selected_end],
            FlatRow::Detail(_) | FlatRow::Description(..)
        )
    {
        selected_end += 1;
    }
    if selected_row < state.scroll {
        state.scroll = selected_row;
    }
    if selected_end > state.scroll + visible_height {
        state.scroll = if selected_end - selected_row > visible_height {
            selected_row
        } else {
            selected_end - visible_height
        };
    }
    let max_scroll = rows.len().saturating_sub(visible_height);
    if state.scroll > max_scroll {
        state.scroll = max_scroll;
    }
    let end = (state.scroll + visible_height).min(rows.len());
    for (vi, ri) in (state.scroll..end).enumerate() {
        let row_y = y + vi as u16;
        if row_y >= content_area.y + content_area.height {
            break;
        }
        match &rows[ri] {
            FlatRow::GroupHeader(group) => {
                let label = match group {
                    AgentGroup::Scope(AgentScope::BuiltIn) => {
                        "\u{2500}\u{2500} Built-in \u{2500}\u{2500}"
                    }
                    AgentGroup::Scope(AgentScope::Project) => {
                        "\u{2500}\u{2500} Project \u{2500}\u{2500}"
                    }
                    AgentGroup::Scope(AgentScope::User) => "\u{2500}\u{2500} User \u{2500}\u{2500}",
                    AgentGroup::Scope(AgentScope::Bundled) => {
                        "\u{2500}\u{2500} Bundled \u{2500}\u{2500}"
                    }
                    AgentGroup::Plugin => "\u{2500}\u{2500} Plugins \u{2500}\u{2500}",
                };
                let style = Style::default()
                    .fg(theme.gray_dim)
                    .add_modifier(Modifier::BOLD);
                buf.set_string(content_area.x, row_y, label, style);
            }
            FlatRow::Agent(idx) => {
                state.row_map.push((row_y, *idx));
                let entry = &state.agents[*idx];
                let is_selected = *idx == state.selected;
                let bg = if is_selected {
                    Some(theme.bg_highlight)
                } else {
                    None
                };
                if let Some(bg_color) = bg {
                    let bg_style = Style::default().bg(bg_color);
                    for x in content_area.x..content_area.x + content_area.width {
                        if let Some(cell) = buf.cell_mut((x, row_y)) {
                            cell.set_style(bg_style);
                        }
                    }
                }
                let mut x = content_area.x;
                let indicator = if entry.expanded {
                    "\u{25bc} "
                } else {
                    "\u{25b6} "
                };
                let ind_style = Style::default().fg(theme.gray_dim);
                let ind_style = if let Some(bg_color) = bg {
                    ind_style.bg(bg_color)
                } else {
                    ind_style
                };
                buf.set_string(x, row_y, indicator, ind_style);
                x += 2;
                let status = if entry.enabled {
                    format!("{} ", crate::glyphs::filled_dot())
                } else {
                    "\u{25cb} ".to_string()
                };
                let status_fg = if entry.enabled {
                    theme.accent_success
                } else {
                    theme.gray_dim
                };
                let status_style = Style::default().fg(status_fg);
                let status_style = if let Some(bg_color) = bg {
                    status_style.bg(bg_color)
                } else {
                    status_style
                };
                buf.set_string(x, row_y, status, status_style);
                x += 2;
                let name_w = entry.name.width();
                let remaining = (content_area.x + content_area.width).saturating_sub(x) as usize;
                let name_display: String = entry.name.chars().take(remaining).collect();
                let mut name_style = Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD);
                if let Some(bg_color) = bg {
                    name_style = name_style.bg(bg_color);
                }
                buf.set_string(x, row_y, &name_display, name_style);
                x += name_w.min(remaining) as u16;
                let is_active = state
                    .active_agent
                    .as_deref()
                    .is_some_and(|a| a == entry.name);
                if is_active {
                    let active_label = " active";
                    let active_remaining =
                        (content_area.x + content_area.width).saturating_sub(x) as usize;
                    if active_remaining >= active_label.width() {
                        let mut active_style = Style::default()
                            .fg(theme.accent_success)
                            .add_modifier(Modifier::BOLD);
                        if let Some(bg_color) = bg {
                            active_style = active_style.bg(bg_color);
                        }
                        buf.set_string(x, row_y, active_label, active_style);
                        x += active_label.width() as u16;
                    }
                }
                let is_default = entry.name == state.default_agent;
                if is_default {
                    let default_label = " default";
                    let default_remaining =
                        (content_area.x + content_area.width).saturating_sub(x) as usize;
                    if default_remaining >= default_label.width() {
                        let mut default_style = Style::default()
                            .fg(theme.text_primary)
                            .add_modifier(Modifier::DIM | Modifier::BOLD);
                        if let Some(bg_color) = bg {
                            default_style = default_style.bg(bg_color);
                        }
                        buf.set_string(x, row_y, default_label, default_style);
                        x += default_label.width() as u16;
                    }
                }
                if !entry.enabled {
                    let off_label = " [off]";
                    let off_remaining =
                        (content_area.x + content_area.width).saturating_sub(x) as usize;
                    if off_remaining >= off_label.len() {
                        let mut off_style = Style::default().fg(theme.gray_dim);
                        if let Some(bg_color) = bg {
                            off_style = off_style.bg(bg_color);
                        }
                        buf.set_string(x, row_y, off_label, off_style);
                        x += off_label.len() as u16;
                    }
                }
                let (badge_text, mut badge_style) = if entry.definition.plugin_name.is_some() {
                    (
                        " plugin ".to_string(),
                        Style::default().fg(theme.text_secondary),
                    )
                } else {
                    scope_badge(entry.scope, theme)
                };
                if let Some(bg_color) = bg {
                    badge_style = badge_style.bg(bg_color);
                }
                let badge_remaining =
                    (content_area.x + content_area.width).saturating_sub(x + 1) as usize;
                if badge_remaining >= badge_text.width() {
                    buf.set_string(x + 1, row_y, &badge_text, badge_style);
                }
            }
            FlatRow::Description(idx, line) => {
                state.row_map.push((row_y, *idx));
                let is_selected = *idx == state.selected;
                let bg = if is_selected {
                    Some(theme.bg_highlight)
                } else {
                    None
                };
                let indent = 6u16;
                let desc_x = content_area.x + indent;
                let mut desc_style = Style::default().fg(theme.gray);
                if let Some(bg_color) = bg {
                    desc_style = desc_style.bg(bg_color);
                    let fill = Style::default().bg(bg_color);
                    for cx in content_area.x..content_area.x + content_area.width {
                        buf[(cx, row_y)].set_style(fill);
                    }
                }
                buf.set_string(desc_x, row_y, line, desc_style);
            }
            FlatRow::Detail(text) => {
                let detail_style = Style::default().fg(theme.gray);
                let display: String = text.chars().take(w).collect();
                buf.set_string(content_area.x, row_y, &display, detail_style);
            }
        }
    }
}
/// Render the Personas tab content.
fn render_personas_tab(
    buf: &mut Buffer,
    content_area: &Rect,
    state: &mut AgentsModalState,
    theme: &Theme,
) {
    if let Some(ref input) = state.persona_input {
        render_persona_create_form(
            buf,
            content_area,
            input,
            state.message.as_ref().map(|m| m.text.as_str()),
            theme,
        );
        return;
    }
    if let Some(ref confirm) = state.persona_confirm {
        render_persona_confirm_dialog(buf, content_area, confirm, theme);
        return;
    }
    let mut y = content_area.y;
    let w = content_area.width as usize;
    if let Some(ref msg) = state.message {
        y = render_modal_message_line(buf, content_area.x, y, w, msg, theme);
    }
    let blurb = "Personas shape subagent behavior via the persona parameter on spawn_subagent.";
    let blurb_style = Style::default().fg(theme.gray_dim);
    buf.set_string(content_area.x, y, blurb, blurb_style);
    y += 1;
    let blurb2 = "Used by skills (e.g. /implement) and by the model when spawning subagents.";
    buf.set_string(content_area.x, y, blurb2, blurb_style);
    y += 2;
    if state.search_active || !state.search_query().is_empty() {
        render_agents_search(
            buf,
            Rect::new(content_area.x, y, content_area.width, 1),
            state.search_editor(),
            state.search_active,
            theme,
        );
        y += 1;
        y += 1;
    }
    let visible_height = content_area.height.saturating_sub(y - content_area.y) as usize;
    if visible_height == 0 {
        return;
    }
    let filtered = state.filtered_persona_indices();
    if filtered.is_empty() {
        let msg = if state.personas.is_empty() {
            "No personas available"
        } else {
            "No matching personas"
        };
        buf.set_string(content_area.x, y, msg, Style::default().fg(theme.gray_dim));
        return;
    }
    let mut rows: Vec<PersonaFlatRow> = Vec::new();
    for &idx in &filtered {
        rows.push(PersonaFlatRow::Name(idx));
        let persona = &state.personas[idx];
        let is_expanded = state.persona_expanded.contains(&idx);
        if is_expanded {
            if let Some(ref desc) = persona.description
                && !desc.is_empty()
            {
                let indent = 4usize;
                let desc_w = w.saturating_sub(indent);
                if desc_w > 0 {
                    for line in word_wrap(desc, desc_w) {
                        rows.push(PersonaFlatRow::Description(idx, line));
                    }
                }
            }
            if persona.has_inputs || persona.has_outputs {
                let mut tags = Vec::new();
                if persona.has_inputs {
                    tags.push("accepts structured inputs");
                }
                if persona.has_outputs {
                    tags.push("produces structured outputs");
                }
                rows.push(PersonaFlatRow::Tags(idx, tags.join(" \u{00b7} ")));
            }
            rows.push(PersonaFlatRow::Hint(
                idx,
                "Enter to view full definition".to_string(),
            ));
        }
    }
    let selected_row = rows
        .iter()
        .position(|r| matches!(r, PersonaFlatRow::Name(i) if *i == state.persona_selected))
        .unwrap_or(0);
    let mut selected_end = selected_row + 1;
    while selected_end < rows.len()
        && matches!(
            rows[selected_end],
            PersonaFlatRow::Description(..) | PersonaFlatRow::Tags(..) | PersonaFlatRow::Hint(..)
        )
    {
        selected_end += 1;
    }
    if selected_row < state.persona_scroll {
        state.persona_scroll = selected_row;
    }
    if selected_end > state.persona_scroll + visible_height {
        state.persona_scroll = if selected_end - selected_row > visible_height {
            selected_row
        } else {
            selected_end - visible_height
        };
    }
    let max_scroll = rows.len().saturating_sub(visible_height);
    if state.persona_scroll > max_scroll {
        state.persona_scroll = max_scroll;
    }
    let end = (state.persona_scroll + visible_height).min(rows.len());
    for (vi, ri) in (state.persona_scroll..end).enumerate() {
        let row_y = y + vi as u16;
        if row_y >= content_area.y + content_area.height {
            break;
        }
        match &rows[ri] {
            PersonaFlatRow::Name(idx) => {
                state.row_map.push((row_y, *idx));
                let is_selected = *idx == state.persona_selected;
                let is_expanded = state.persona_expanded.contains(idx);
                let bg = if is_selected {
                    Some(theme.bg_highlight)
                } else {
                    None
                };
                if let Some(bg_color) = bg {
                    let bg_style = Style::default().bg(bg_color);
                    for x in content_area.x..content_area.x + content_area.width {
                        if let Some(cell) = buf.cell_mut((x, row_y)) {
                            cell.set_style(bg_style);
                        }
                    }
                }
                let mut x = content_area.x;
                let indicator = if is_expanded {
                    "\u{25bc} "
                } else {
                    "\u{25b6} "
                };
                let mut ind_style = Style::default().fg(theme.gray_dim);
                if let Some(bg_color) = bg {
                    ind_style = ind_style.bg(bg_color);
                }
                buf.set_string(x, row_y, indicator, ind_style);
                x += 2;
                let persona = &state.personas[*idx];
                let remaining = (content_area.x + content_area.width).saturating_sub(x) as usize;
                let name_display: String = persona.name.chars().take(remaining).collect();
                let mut name_style = Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD);
                if let Some(bg_color) = bg {
                    name_style = name_style.bg(bg_color);
                }
                buf.set_string(x, row_y, &name_display, name_style);
                x += name_display.width() as u16;
                if let Some(ref scope) = persona.scope_label {
                    let badge = format!(" {scope} ");
                    let mut scope_style = Style::default().fg(theme.accent_user);
                    if let Some(bg_color) = bg {
                        scope_style = scope_style.bg(bg_color);
                    }
                    buf.set_string(x, row_y, &badge, scope_style);
                    x += badge.width() as u16;
                }
                if !is_expanded
                    && let Some(ref desc) = persona.description
                    && !desc.is_empty()
                {
                    let sep = " \u{00b7} ";
                    let desc_remaining =
                        (content_area.x + content_area.width).saturating_sub(x) as usize;
                    if desc_remaining > sep.width() + 3 {
                        let mut desc_style = Style::default().fg(theme.gray);
                        if let Some(bg_color) = bg {
                            desc_style = desc_style.bg(bg_color);
                        }
                        buf.set_string(x, row_y, sep, desc_style);
                        x += sep.width() as u16;
                        let max_desc =
                            (content_area.x + content_area.width).saturating_sub(x) as usize;
                        let truncated: String = desc.chars().take(max_desc).collect();
                        buf.set_string(x, row_y, &truncated, desc_style);
                    }
                }
            }
            PersonaFlatRow::Description(idx, line) => {
                state.row_map.push((row_y, *idx));
                let is_selected = *idx == state.persona_selected;
                let bg = if is_selected {
                    Some(theme.bg_highlight)
                } else {
                    None
                };
                let indent = 4u16;
                let desc_x = content_area.x + indent;
                let mut desc_style = Style::default().fg(theme.gray);
                if let Some(bg_color) = bg {
                    desc_style = desc_style.bg(bg_color);
                    let fill = Style::default().bg(bg_color);
                    for cx in content_area.x..content_area.x + content_area.width {
                        if let Some(cell) = buf.cell_mut((cx, row_y)) {
                            cell.set_style(fill);
                        }
                    }
                }
                buf.set_string(desc_x, row_y, line, desc_style);
            }
            PersonaFlatRow::Tags(idx, tags) => {
                state.row_map.push((row_y, *idx));
                let is_selected = *idx == state.persona_selected;
                let bg = if is_selected {
                    Some(theme.bg_highlight)
                } else {
                    None
                };
                let indent = 4u16;
                let tag_x = content_area.x + indent;
                let mut tag_style = Style::default().fg(theme.gray_dim);
                if let Some(bg_color) = bg {
                    tag_style = tag_style.bg(bg_color);
                    let fill = Style::default().bg(bg_color);
                    for cx in content_area.x..content_area.x + content_area.width {
                        if let Some(cell) = buf.cell_mut((cx, row_y)) {
                            cell.set_style(fill);
                        }
                    }
                }
                let display = format!("[{tags}]");
                buf.set_string(tag_x, row_y, &display, tag_style);
            }
            PersonaFlatRow::Hint(idx, text) => {
                state.row_map.push((row_y, *idx));
                let is_selected = *idx == state.persona_selected;
                let bg = if is_selected {
                    Some(theme.bg_highlight)
                } else {
                    None
                };
                let indent = 4u16;
                let hint_x = content_area.x + indent;
                let mut hint_style = Style::default().fg(theme.gray_dim);
                if let Some(bg_color) = bg {
                    hint_style = hint_style.bg(bg_color);
                    let fill = Style::default().bg(bg_color);
                    for cx in content_area.x..content_area.x + content_area.width {
                        if let Some(cell) = buf.cell_mut((cx, row_y)) {
                            cell.set_style(fill);
                        }
                    }
                }
                buf.set_string(hint_x, row_y, text, hint_style);
            }
        }
    }
}
/// Flat row types for the personas tab.
enum PersonaFlatRow {
    Name(usize),
    Description(usize, String),
    Tags(usize, String),
    Hint(usize, String),
}
fn next_persona_create_field(field: CreateField) -> CreateField {
    match field {
        CreateField::Name => CreateField::Description,
        CreateField::Description => CreateField::Instructions,
        CreateField::Instructions => CreateField::Scope,
        CreateField::Scope => CreateField::Name,
    }
}
fn prev_persona_create_field(field: CreateField) -> CreateField {
    match field {
        CreateField::Name => CreateField::Scope,
        CreateField::Description => CreateField::Name,
        CreateField::Instructions => CreateField::Description,
        CreateField::Scope => CreateField::Instructions,
    }
}
#[allow(clippy::too_many_arguments)]
fn render_create_text_field(
    buf: &mut Buffer,
    content_area: &Rect,
    y: u16,
    w: usize,
    label: &str,
    editor: &LineEditor,
    active: bool,
    theme: &Theme,
) -> u16 {
    let label_style = if active {
        Style::default().fg(theme.accent_user)
    } else {
        Style::default().fg(theme.gray)
    };
    buf.set_string(content_area.x, y, label, label_style);
    let label_width = label.width();
    let field_x = content_area.x + label_width as u16;
    let remaining = w.saturating_sub(label_width);
    let viewport = editor.viewport(remaining);
    let leading;
    let display: &str = if active {
        &editor.text()[viewport.visible_byte_range.clone()]
    } else {
        leading = crate::render::line_utils::truncate_str(editor.text(), remaining);
        &leading
    };
    let field_style = Style::default().fg(theme.text_primary);
    buf.set_string(field_x, y, display, field_style);
    if active {
        let cursor_x = field_x + viewport.cursor_display_column as u16;
        if cursor_x < content_area.x + content_area.width
            && let Some(cell) = buf.cell_mut((cursor_x, y))
        {
            cell.set_style(Style::default().fg(theme.bg_base).bg(theme.text_primary));
        }
    }
    y + 2
}
/// Render the create-persona form overlay.
fn render_persona_create_form(
    buf: &mut Buffer,
    content_area: &Rect,
    input: &PersonaCreateInput,
    message: Option<&str>,
    theme: &Theme,
) {
    let mut y = content_area.y;
    let w = content_area.width as usize;
    let title = "Create New Persona";
    let title_style = Style::default()
        .fg(theme.text_primary)
        .add_modifier(Modifier::BOLD);
    buf.set_string(content_area.x, y, title, title_style);
    y += 2;
    if let Some(msg) = message {
        buf.set_string(
            content_area.x,
            y,
            msg,
            Style::default().fg(theme.accent_error),
        );
        y += 2;
    }
    y = render_create_text_field(
        buf,
        content_area,
        y,
        w,
        "Name: ",
        input.name_editor(),
        input.active_field == CreateField::Name,
        theme,
    );
    y = render_create_text_field(
        buf,
        content_area,
        y,
        w,
        "Description: ",
        input.description_editor(),
        input.active_field == CreateField::Description,
        theme,
    );
    y = render_create_text_field(
        buf,
        content_area,
        y,
        w,
        "Instructions: ",
        input.instructions_editor(),
        input.active_field == CreateField::Instructions,
        theme,
    );
    let scope_label = "Scope: ";
    let scope_active = input.active_field == CreateField::Scope;
    let label_style = if scope_active {
        Style::default().fg(theme.accent_user)
    } else {
        Style::default().fg(theme.gray)
    };
    buf.set_string(content_area.x, y, scope_label, label_style);
    let scope_text = format!("[{}]", input.scope.label());
    buf.set_string(
        content_area.x + scope_label.len() as u16,
        y,
        &scope_text,
        Style::default().fg(theme.text_primary),
    );
    y += 2;
    let hint = "Tab/↑↓: field | Space/←→ on scope: user/project | Enter: create | Esc: cancel";
    buf.set_string(content_area.x, y, hint, Style::default().fg(theme.gray_dim));
}
/// Render the confirm-delete persona dialog.
fn render_persona_confirm_dialog(
    buf: &mut Buffer,
    content_area: &Rect,
    confirm: &PersonaConfirmAction,
    theme: &Theme,
) {
    let PersonaConfirmAction::Delete { name, path } = confirm;
    let mut y = content_area.y;
    let title = "Delete Persona";
    let title_style = Style::default()
        .fg(theme.accent_error)
        .add_modifier(Modifier::BOLD);
    buf.set_string(content_area.x, y, title, title_style);
    y += 2;
    let msg = format!("Delete persona '{name}'?");
    buf.set_string(
        content_area.x,
        y,
        &msg,
        Style::default().fg(theme.text_primary),
    );
    y += 1;
    let path_msg = format!("  {}", path.display());
    buf.set_string(
        content_area.x,
        y,
        &path_msg,
        Style::default().fg(theme.gray),
    );
    y += 2;
    let hint = "y: confirm | n/Esc: cancel";
    buf.set_string(content_area.x, y, hint, Style::default().fg(theme.gray_dim));
}
/// Group an agent entry belongs to in the flat list: its scope, or the
/// dedicated plugins group for plugin-provided agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentGroup {
    Scope(AgentScope),
    Plugin,
}
impl AgentGroup {
    fn of(entry: &AgentListEntry) -> Self {
        if entry.definition.plugin_name.is_some() {
            Self::Plugin
        } else {
            Self::Scope(entry.scope)
        }
    }
}
enum FlatRow {
    GroupHeader(AgentGroup),
    Agent(usize),
    /// Word-wrapped description line, always shown below the agent header row.
    Description(usize, String),
    Detail(String),
}
fn message_line_style(kind: AgentsModalMessageKind, theme: &Theme) -> Style {
    let fg = match kind {
        AgentsModalMessageKind::Error => theme.accent_error,
        AgentsModalMessageKind::Success => theme.accent_success,
        AgentsModalMessageKind::Info => theme.text_secondary,
    };
    Style::default().fg(fg)
}
/// Render an inline message; returns `y` after the message block (including separator).
fn render_modal_message_line(
    buf: &mut Buffer,
    x: u16,
    mut y: u16,
    w: usize,
    msg: &AgentsModalMessage,
    theme: &Theme,
) -> u16 {
    let display: String = msg.text.chars().take(w).collect();
    buf.set_string(x, y, &display, message_line_style(msg.kind, theme));
    y += 1;
    y + 1
}
/// Clear create/confirm overlays belonging to the tab being left.
fn clear_overlays_for_tab(state: &mut AgentsModalState, tab: AgentsTab) {
    match tab {
        AgentsTab::Agents => {
            state.persona_input = None;
            state.persona_confirm = None;
        }
        AgentsTab::Personas => {}
    }
}
fn switch_agents_tab(state: &mut AgentsModalState, tab: AgentsTab) {
    clear_overlays_for_tab(state, tab);
    state.active_tab = tab;
    state.search.reset();
    state.search_active = false;
}
/// Handle a key event while the agents modal is open.
pub fn handle_agents_key(state: &mut AgentsModalState, key: &KeyEvent) -> AgentsModalOutcome {
    state.message = None;
    if state.persona_input.is_some() && state.active_tab == AgentsTab::Personas {
        return handle_persona_create_form_key(state, key);
    }
    if state.persona_confirm.is_some() && state.active_tab == AgentsTab::Personas {
        return handle_persona_confirm_key(state, key);
    }
    if state.search_active {
        if key.code == KeyCode::Esc {
            state.search.reset();
            state.search_active = false;
            return AgentsModalOutcome::Changed;
        }
        if key.code == KeyCode::Enter {
            state.search_active = false;
            return AgentsModalOutcome::Changed;
        }
        if crate::input::key::is_shift_tab(key) {
            let tab = state.active_tab.prev();
            switch_agents_tab(state, tab);
            return AgentsModalOutcome::Changed;
        }
        if crate::input::key::KeyShortcut::key(KeyCode::Tab).matches(key) {
            let tab = state.active_tab.next();
            switch_agents_tab(state, tab);
            return AgentsModalOutcome::Changed;
        }
        let outcome = state.search.handle_key(key);
        return finish_search_edit(state, outcome);
    }
    let tab_labels: Vec<&str> = AgentsTab::ALL.iter().map(|t| t.label()).collect();
    let config = ModalWindowConfig {
        title: "Agents",
        tabs: Some(&tab_labels),
        shortcuts: &[],
        sizing: modal_sizing(false),
        fold_info: None,
    };
    let chrome = modal_window::handle_modal_key(&mut state.window, key, &config);
    match chrome {
        modal_window::ModalWindowOutcome::CloseRequested => {
            return AgentsModalOutcome::Close;
        }
        modal_window::ModalWindowOutcome::TabChanged(idx) => {
            if let Some(&tab) = AgentsTab::ALL.get(idx) {
                switch_agents_tab(state, tab);
            }
            return AgentsModalOutcome::Changed;
        }
        _ => {}
    }
    if crate::input::key::KeyShortcut::key(KeyCode::Tab).matches(key) {
        let tab = state.active_tab.next();
        switch_agents_tab(state, tab);
        return AgentsModalOutcome::Changed;
    }
    if crate::input::key::is_shift_tab(key) {
        let tab = state.active_tab.prev();
        switch_agents_tab(state, tab);
        return AgentsModalOutcome::Changed;
    }
    match state.active_tab {
        AgentsTab::Agents => handle_agents_tab_key(state, key),
        AgentsTab::Personas => handle_personas_tab_key(state, key),
    }
}
pub fn handle_agents_paste(state: &mut AgentsModalState, text: &str) -> AgentsModalOutcome {
    if let Some(input) = state.persona_input.as_mut() {
        let Some(editor) = input.active_editor_mut() else {
            return AgentsModalOutcome::Unchanged;
        };
        let outcome = editor.insert_paste(text);
        if outcome == LineEditOutcome::TextChanged {
            state.message = None;
        }
        return finish_line_edit(outcome);
    }
    if state.search_active {
        let outcome = state.search.insert_paste(text);
        if outcome == LineEditOutcome::TextChanged {
            state.message = None;
        }
        return finish_search_edit(state, outcome);
    }
    AgentsModalOutcome::Unchanged
}
fn finish_search_edit(
    state: &mut AgentsModalState,
    outcome: LineEditOutcome,
) -> AgentsModalOutcome {
    if outcome == LineEditOutcome::TextChanged {
        state.reset_selection_after_search_change();
    }
    finish_line_edit(outcome)
}
fn finish_line_edit(outcome: LineEditOutcome) -> AgentsModalOutcome {
    match outcome {
        LineEditOutcome::TextChanged
        | LineEditOutcome::HandledNoChange
        | LineEditOutcome::CursorChanged => AgentsModalOutcome::Changed,
        LineEditOutcome::Unhandled => AgentsModalOutcome::Unchanged,
    }
}
/// Handle key input specific to the Agents tab.
fn handle_agents_tab_key(state: &mut AgentsModalState, key: &KeyEvent) -> AgentsModalOutcome {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            state.select_next();
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.select_prev();
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('e') | KeyCode::Right => {
            state.expand();
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('E') | KeyCode::Left => {
            state.collapse();
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            for _ in 0..10 {
                state.select_next();
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            for _ in 0..10 {
                state.select_prev();
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::PageDown => {
            for _ in 0..10 {
                state.select_next();
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::PageUp => {
            for _ in 0..10 {
                state.select_prev();
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::Enter | KeyCode::Char('o') => {
            if let Some(entry) = state.agents.get(state.selected) {
                if let Some(ref path) = entry.source_path {
                    let title = format!("{} \u{00b7} prompt extension", entry.name);
                    return AgentsModalOutcome::ViewAgent {
                        title,
                        source_path: Some(path.clone()),
                        content: None,
                    };
                }
                if entry.definition.prompt_body.is_some() {
                    let title = format!("{} \u{00b7} prompt extension", entry.name);
                    return AgentsModalOutcome::ViewAgent {
                        title,
                        source_path: None,
                        content: Some(synthesize_agent_markdown(entry)),
                    };
                }
                AgentsModalOutcome::Unchanged
            } else {
                AgentsModalOutcome::Unchanged
            }
        }
        KeyCode::Char('/') | KeyCode::Char('i') if key.modifiers.is_empty() => {
            state.search_active = true;
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('q') => AgentsModalOutcome::Close,
        KeyCode::Char('s') => {
            if let Some(entry) = state.agents.get(state.selected) {
                if entry.definition.plugin_name.is_some() {
                    state.message = Some(AgentsModalMessage::info(
                        "Plugin agents can't be the session default \u{2014} \
                         they are spawned as subagents via the Task tool.",
                    ));
                    return AgentsModalOutcome::Changed;
                }
                let name = entry.name.clone();
                let is_already_default = load_config_agent_name().as_deref() == Some(name.as_str());
                let new_default = if is_already_default {
                    None
                } else {
                    Some(name.as_str())
                };
                match set_default_agent(new_default) {
                    Ok(()) => {
                        refresh_default_agent(state);
                        state.message = Some(if is_already_default {
                            AgentsModalMessage::info(format!(
                                "Cleared: new sessions use '{}'",
                                state.default_agent
                            ))
                        } else {
                            AgentsModalMessage::info(format!(
                                "New sessions will start with '{}'",
                                state.default_agent
                            ))
                        });
                    }
                    Err(e) => {
                        state.message = Some(AgentsModalMessage::error(e));
                    }
                }
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('t') => {
            if let Some(entry) = state.agents.get(state.selected) {
                let new_enabled = !entry.enabled;
                let name = entry.name.clone();
                match toggle_agent(&name, new_enabled) {
                    Ok(()) => {
                        state.rebuild_agents();
                        state.message = Some(AgentsModalMessage::info(format!(
                            "{} '{}' \u{2014} applies to new sessions",
                            if new_enabled { "Enabled" } else { "Disabled" },
                            name
                        )));
                    }
                    Err(e) => {
                        state.message = Some(AgentsModalMessage::error(e));
                    }
                }
            }
            AgentsModalOutcome::Changed
        }
        _ => AgentsModalOutcome::Unchanged,
    }
}
/// Handle key input specific to the Personas tab.
fn handle_personas_tab_key(state: &mut AgentsModalState, key: &KeyEvent) -> AgentsModalOutcome {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            state.persona_select_next();
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.persona_select_prev();
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('e') | KeyCode::Right => {
            state.persona_expanded.insert(state.persona_selected);
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('E') | KeyCode::Left => {
            state.persona_expanded.remove(&state.persona_selected);
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            for _ in 0..10 {
                state.persona_select_next();
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            for _ in 0..10 {
                state.persona_select_prev();
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::PageDown => {
            for _ in 0..10 {
                state.persona_select_next();
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::PageUp => {
            for _ in 0..10 {
                state.persona_select_prev();
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::Enter | KeyCode::Char('o') => {
            if let Some(persona) = state.personas.get(state.persona_selected) {
                let source_path = persona.source_path.as_ref().map(PathBuf::from);
                let editable = persona_is_editable(persona);
                let scope_label = persona
                    .scope_label
                    .clone()
                    .unwrap_or_else(|| "bundled".to_string());
                return AgentsModalOutcome::OpenPersonaDetail {
                    name: persona.name.clone(),
                    source_path,
                    editable,
                    scope_label,
                };
            }
            AgentsModalOutcome::Unchanged
        }
        KeyCode::Char('n') => {
            state.persona_input = Some(PersonaCreateInput::new());
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('d') => {
            if let Some(persona) = state.personas.get(state.persona_selected) {
                if !persona_is_deletable(persona) {
                    state.message =
                        Some(AgentsModalMessage::error("Cannot delete bundled personas"));
                    return AgentsModalOutcome::Changed;
                }
                if let Some(ref path_str) = persona.source_path {
                    state.persona_confirm = Some(PersonaConfirmAction::Delete {
                        name: persona.name.clone(),
                        path: PathBuf::from(path_str),
                    });
                } else {
                    state.message = Some(AgentsModalMessage::error("Persona has no source file"));
                }
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('/') | KeyCode::Char('i') if key.modifiers.is_empty() => {
            state.search_active = true;
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('q') => AgentsModalOutcome::Close,
        _ => AgentsModalOutcome::Unchanged,
    }
}
fn handle_persona_create_form_tab_key(active_field: CreateField, back_tab: bool) -> CreateField {
    if back_tab {
        prev_persona_create_field(active_field)
    } else {
        next_persona_create_field(active_field)
    }
}
fn try_toggle_create_scope(
    active_field: CreateField,
    scope: &mut ConfigFileScope,
    key: &KeyEvent,
) -> bool {
    if active_field != CreateField::Scope {
        return false;
    }
    let toggle = matches!(
        key.code,
        KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
    ) && key.modifiers.is_empty();
    if toggle {
        *scope = scope.toggle();
    }
    toggle
}
fn persona_create_form_field_nav(active_field: CreateField, key: &KeyEvent) -> Option<CreateField> {
    if !key.modifiers.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(prev_persona_create_field(active_field)),
        KeyCode::Down => Some(next_persona_create_field(active_field)),
        _ => None,
    }
}
fn persona_create_form_field_nav_scroll(
    active_field: CreateField,
    scroll_down: bool,
) -> CreateField {
    if scroll_down {
        next_persona_create_field(active_field)
    } else {
        prev_persona_create_field(active_field)
    }
}
/// Handle key input in the persona create form.
fn handle_persona_create_form_key(
    state: &mut AgentsModalState,
    key: &KeyEvent,
) -> AgentsModalOutcome {
    let Some(input) = state.persona_input.as_mut() else {
        return AgentsModalOutcome::Unchanged;
    };
    let cwd = state.cwd.clone();
    if key.code == KeyCode::Esc {
        state.persona_input = None;
        return AgentsModalOutcome::Changed;
    }
    if crate::input::key::is_shift_tab(key) {
        input.active_field = handle_persona_create_form_tab_key(input.active_field, true);
        return AgentsModalOutcome::Changed;
    }
    if crate::input::key::KeyShortcut::key(KeyCode::Tab).matches(key) {
        input.active_field = handle_persona_create_form_tab_key(input.active_field, false);
        return AgentsModalOutcome::Changed;
    }
    if try_toggle_create_scope(input.active_field, &mut input.scope, key) {
        return AgentsModalOutcome::Changed;
    }
    if let Some(field) = persona_create_form_field_nav(input.active_field, key) {
        input.active_field = field;
        return AgentsModalOutcome::Changed;
    }
    if key.code == KeyCode::Enter {
        let name = input.name().trim().to_string();
        let description = input.description().trim().to_string();
        let instructions = input.instructions().trim().to_string();
        let scope = input.scope;
        if name.is_empty() {
            state.message = Some(AgentsModalMessage::error("Name is required"));
            return AgentsModalOutcome::Changed;
        }
        match create_persona_template(&name, &description, &instructions, scope, &cwd) {
            Ok(path) => {
                let label = path.file_stem().and_then(|s| s.to_str()).unwrap_or(&name);
                state.persona_input = None;
                state.refresh_personas();
                state.message = Some(AgentsModalMessage::success(format!(
                    "Created persona '{label}'"
                )));
            }
            Err(e) => {
                state.message = Some(AgentsModalMessage::error(e));
            }
        }
        return AgentsModalOutcome::Changed;
    }
    let Some(editor) = input.active_editor_mut() else {
        return AgentsModalOutcome::Unchanged;
    };
    finish_line_edit(editor.handle_key(key))
}
/// Handle key input in the persona confirm dialog.
fn handle_persona_confirm_key(state: &mut AgentsModalState, key: &KeyEvent) -> AgentsModalOutcome {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(confirm) = state.persona_confirm.take() else {
                return AgentsModalOutcome::Unchanged;
            };
            let PersonaConfirmAction::Delete { name, path } = confirm;
            match delete_persona_file(&path) {
                Ok(()) => {
                    state.refresh_personas();
                    state.message = Some(AgentsModalMessage::success(format!(
                        "Deleted persona '{name}'"
                    )));
                }
                Err(e) => {
                    state.message = Some(AgentsModalMessage::error(e));
                }
            }
            AgentsModalOutcome::Changed
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            state.persona_confirm = None;
            AgentsModalOutcome::Changed
        }
        _ => AgentsModalOutcome::Unchanged,
    }
}
/// Handle a mouse event while the agents modal is open.
pub fn handle_agents_mouse(state: &mut AgentsModalState, mouse: &MouseEvent) -> AgentsModalOutcome {
    let chrome =
        modal_window::handle_modal_mouse(&mut state.window, mouse.kind, mouse.column, mouse.row);
    match chrome {
        modal_window::ModalWindowOutcome::CloseRequested => AgentsModalOutcome::Close,
        modal_window::ModalWindowOutcome::TabChanged(idx) => {
            if let Some(&tab) = AgentsTab::ALL.get(idx) {
                switch_agents_tab(state, tab);
            }
            AgentsModalOutcome::Changed
        }
        modal_window::ModalWindowOutcome::Handled => AgentsModalOutcome::Changed,
        _ => {
            let in_content = state.content_rect.is_some_and(|r| {
                r.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
            });
            if in_content
                && state.active_tab == AgentsTab::Personas
                && let Some(input) = state.persona_input.as_mut()
            {
                match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        input.active_field =
                            persona_create_form_field_nav_scroll(input.active_field, true);
                        return AgentsModalOutcome::Changed;
                    }
                    MouseEventKind::ScrollUp => {
                        input.active_field =
                            persona_create_form_field_nav_scroll(input.active_field, false);
                        return AgentsModalOutcome::Changed;
                    }
                    _ => {}
                }
            }
            match state.active_tab {
                AgentsTab::Agents => match mouse.kind {
                    MouseEventKind::ScrollUp if in_content => {
                        state.select_prev();
                        AgentsModalOutcome::Changed
                    }
                    MouseEventKind::ScrollDown if in_content => {
                        state.select_next();
                        AgentsModalOutcome::Changed
                    }
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) if in_content => {
                        if let Some(&(_, agent_idx)) =
                            state.row_map.iter().find(|(y, _)| *y == mouse.row)
                        {
                            if agent_idx == state.selected {
                                if state.agents.get(agent_idx).is_some_and(|e| e.expanded) {
                                    state.collapse();
                                } else {
                                    state.expand();
                                }
                            } else {
                                state.selected = agent_idx;
                            }
                            AgentsModalOutcome::Changed
                        } else {
                            AgentsModalOutcome::Unchanged
                        }
                    }
                    _ => AgentsModalOutcome::Unchanged,
                },
                AgentsTab::Personas => match mouse.kind {
                    MouseEventKind::ScrollUp if in_content => {
                        state.persona_select_prev();
                        AgentsModalOutcome::Changed
                    }
                    MouseEventKind::ScrollDown if in_content => {
                        state.persona_select_next();
                        AgentsModalOutcome::Changed
                    }
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) if in_content => {
                        if let Some(&(_, persona_idx)) =
                            state.row_map.iter().find(|(y, _)| *y == mouse.row)
                        {
                            if persona_idx == state.persona_selected {
                                if state.persona_expanded.contains(&persona_idx) {
                                    state.persona_expanded.remove(&persona_idx);
                                } else {
                                    state.persona_expanded.insert(persona_idx);
                                }
                            } else {
                                state.persona_selected = persona_idx;
                            }
                            AgentsModalOutcome::Changed
                        } else {
                            AgentsModalOutcome::Unchanged
                        }
                    }
                    _ => AgentsModalOutcome::Unchanged,
                },
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_runtime::agent::config::DEFAULT_AGENT_TYPE;
    #[test]
    fn agents_tab_next_cycles() {
        assert_eq!(AgentsTab::Agents.next(), AgentsTab::Personas);
        assert_eq!(AgentsTab::Personas.next(), AgentsTab::Agents);
    }
    #[test]
    fn agents_tab_prev_cycles() {
        assert_eq!(AgentsTab::Agents.prev(), AgentsTab::Personas);
        assert_eq!(AgentsTab::Personas.prev(), AgentsTab::Agents);
    }
    #[test]
    fn agents_tab_all_covers_variants() {
        assert_eq!(AgentsTab::ALL.len(), 2);
        assert_eq!(AgentsTab::ALL[0], AgentsTab::Agents);
        assert_eq!(AgentsTab::ALL[1], AgentsTab::Personas);
    }
    #[test]
    fn agents_tab_labels_nonempty() {
        for tab in AgentsTab::ALL {
            assert!(!tab.label().is_empty());
        }
    }
    #[test]
    fn agents_tab_next_prev_roundtrip() {
        for &tab in AgentsTab::ALL {
            assert_eq!(tab.next().prev(), tab);
            assert_eq!(tab.prev().next(), tab);
        }
    }
    #[test]
    fn build_persona_list_from_details() {
        let bundle = BundleState {
            persona_details: vec![
                PersonaDetail {
                    name: "researcher".to_string(),
                    description: Some("thorough researcher".to_string()),
                    has_inputs: true,
                    has_outputs: false,
                    source_path: None,
                    scope_label: None,
                },
                PersonaDetail {
                    name: "auditor".to_string(),
                    description: None,
                    has_inputs: false,
                    has_outputs: true,
                    source_path: None,
                    scope_label: None,
                },
            ],
            personas: vec!["ignored".to_string()],
            ..Default::default()
        };
        let list = merge_persona_lists(&bundle, Path::new("/tmp"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "researcher");
        assert_eq!(list[0].description.as_deref(), Some("thorough researcher"));
        assert!(list[0].has_inputs);
        assert!(!list[0].has_outputs);
        assert_eq!(list[1].name, "auditor");
        assert!(list[1].description.is_none());
        assert!(!list[1].has_inputs);
        assert!(list[1].has_outputs);
    }
    #[test]
    fn build_persona_list_fallback_to_names() {
        let bundle = BundleState {
            personas: vec!["alpha".to_string(), "beta".to_string()],
            persona_details: vec![],
            ..Default::default()
        };
        let list = merge_persona_lists(&bundle, Path::new("/tmp"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "alpha");
        assert!(list[0].description.is_none());
        assert!(!list[0].has_inputs);
        assert!(!list[0].has_outputs);
        assert_eq!(list[1].name, "beta");
    }
    #[test]
    fn build_persona_list_empty_bundle() {
        let bundle = BundleState::default();
        let list = merge_persona_lists(&bundle, Path::new("/tmp"));
        assert!(list.is_empty());
    }
    #[test]
    fn merge_persona_lists_appends_local_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let personas_dir = dir.path().join(".echo-agent").join("personas");
        std::fs::create_dir_all(&personas_dir).expect("mkdir");
        std::fs::write(
            personas_dir.join("local-only.toml"),
            "instructions = \"be local\"\n",
        )
        .expect("write");
        let bundle = BundleState {
            persona_details: vec![PersonaDetail {
                name: "bundled-one".to_string(),
                description: None,
                has_inputs: false,
                has_outputs: false,
                source_path: None,
                scope_label: None,
            }],
            ..Default::default()
        };
        let list = merge_persona_lists(&bundle, dir.path());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "bundled-one");
        assert_eq!(list[1].name, "local-only");
        assert_eq!(list[1].scope_label.as_deref(), Some("project"));
        assert!(list[1].source_path.is_some());
    }
    #[test]
    fn create_persona_template_project_scope_writes_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = create_persona_template(
            "helper",
            "helps",
            "always be helpful",
            ConfigFileScope::Project,
            dir.path(),
        )
        .expect("create");
        assert!(path.ends_with("helper.toml"));
        let content = std::fs::read_to_string(&path).expect("read");
        assert!(content.contains("always be helpful"));
        assert!(content.contains("helps"));
    }
    #[test]
    fn sanitize_config_name_rejects_empty_alphanumeric() {
        assert!(sanitize_config_name("---").is_err());
        assert_eq!(sanitize_config_name("my-agent").unwrap(), "my-agent");
        assert_eq!(sanitize_config_name("a b").unwrap(), "a-b");
    }
    #[test]
    fn create_persona_template_empty_instructions_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = create_persona_template(
            "minimal",
            "just a name",
            "",
            ConfigFileScope::Project,
            dir.path(),
        )
        .expect("create");
        let content = std::fs::read_to_string(&path).expect("read");
        assert!(content.contains("just a name"));
        assert!(!content.contains("instructions"));
    }
    #[test]
    fn persona_is_deletable_local_vs_bundled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir
            .path()
            .join(".echo-agent")
            .join("personas")
            .join("p.toml");
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, "instructions = \"x\"\n").unwrap();
        let local_detail = PersonaDetail {
            name: "p".into(),
            description: None,
            has_inputs: false,
            has_outputs: false,
            source_path: Some(local.display().to_string()),
            scope_label: Some("project".into()),
        };
        assert!(persona_is_deletable(&local_detail));
        let bundled_detail = PersonaDetail {
            name: "b".into(),
            description: None,
            has_inputs: false,
            has_outputs: false,
            source_path: Some("/home/user/.echo-agent/bundled/personas/b.toml".into()),
            scope_label: None,
        };
        assert!(!persona_is_deletable(&bundled_detail));
    }
    #[test]
    fn delete_persona_file_rejects_outside_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = dir.path().join("evil.toml");
        std::fs::write(&outside, "instructions = \"x\"\n").unwrap();
        assert!(delete_persona_file(&outside).is_err());
    }
    #[test]
    fn delete_persona_file_allows_project_persona() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .join(".echo-agent")
            .join("personas")
            .join("gone.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "instructions = \"bye\"\n").unwrap();
        delete_persona_file(&path).expect("delete");
        assert!(!path.exists());
    }
    #[test]
    fn filtered_persona_indices_matches_name_and_description() {
        let bundle = BundleState {
            persona_details: vec![
                PersonaDetail {
                    name: "researcher".to_string(),
                    description: Some("finds info".to_string()),
                    has_inputs: false,
                    has_outputs: false,
                    source_path: None,
                    scope_label: None,
                },
                PersonaDetail {
                    name: "auditor".to_string(),
                    description: Some("reviews code".to_string()),
                    has_inputs: false,
                    has_outputs: false,
                    source_path: None,
                    scope_label: None,
                },
            ],
            ..Default::default()
        };
        let personas = merge_persona_lists(&bundle, Path::new("/tmp"));
        let make_state = |query: &str| -> AgentsModalState {
            let mut state = AgentsModalState {
                window: ModalWindowState::with_tabs(2),
                active_tab: AgentsTab::Personas,
                agents: Vec::new(),
                selected: 0,
                scroll: 0,
                search: LineEditor::default(),
                search_active: false,
                row_map: Vec::new(),
                content_rect: None,
                persona_input: None,
                persona_confirm: None,
                message: None,
                cwd: PathBuf::new(),
                bundle: bundle.clone(),
                default_agent: DEFAULT_AGENT_TYPE.to_string(),
                active_agent: None,
                model_agent_type: None,
                plugin_registry: None,
                personas: personas.clone(),
                persona_selected: 0,
                persona_scroll: 0,
                persona_expanded: std::collections::HashSet::new(),
            };
            state.set_search_query(query);
            state
        };
        let s = make_state("");
        assert_eq!(s.filtered_persona_indices(), vec![0, 1]);
        let s = make_state("audit");
        assert_eq!(s.filtered_persona_indices(), vec![1]);
        let s = make_state("finds");
        assert_eq!(s.filtered_persona_indices(), vec![0]);
        let s = make_state("zzzzz");
        assert!(s.filtered_persona_indices().is_empty());
    }
    /// Helper: build a minimal `AgentsModalState` for persona navigation tests.
    fn make_persona_state(
        personas: Vec<PersonaDetail>,
        query: &str,
        selected: usize,
    ) -> AgentsModalState {
        let mut state = AgentsModalState {
            window: ModalWindowState::with_tabs(2),
            active_tab: AgentsTab::Personas,
            agents: Vec::new(),
            selected: 0,
            scroll: 0,
            search: LineEditor::default(),
            search_active: false,
            row_map: Vec::new(),
            content_rect: None,
            persona_input: None,
            persona_confirm: None,
            message: None,
            cwd: PathBuf::new(),
            bundle: BundleState::default(),
            default_agent: DEFAULT_AGENT_TYPE.to_string(),
            active_agent: None,
            model_agent_type: None,
            plugin_registry: None,
            personas,
            persona_selected: selected,
            persona_scroll: 0,
            persona_expanded: std::collections::HashSet::new(),
        };
        state.set_search_query(query);
        state
    }
    fn three_personas() -> Vec<PersonaDetail> {
        vec![
            PersonaDetail {
                name: "alpha".to_string(),
                description: Some("first".to_string()),
                has_inputs: false,
                has_outputs: false,
                source_path: None,
                scope_label: None,
            },
            PersonaDetail {
                name: "beta".to_string(),
                description: Some("second".to_string()),
                has_inputs: false,
                has_outputs: false,
                source_path: None,
                scope_label: None,
            },
            PersonaDetail {
                name: "gamma".to_string(),
                description: Some("third".to_string()),
                has_inputs: false,
                has_outputs: false,
                source_path: None,
                scope_label: None,
            },
        ]
    }
    #[test]
    fn persona_select_next_advances() {
        let mut s = make_persona_state(three_personas(), "", 0);
        s.persona_select_next();
        assert_eq!(s.persona_selected, 1);
        s.persona_select_next();
        assert_eq!(s.persona_selected, 2);
    }
    #[test]
    fn persona_select_next_clamps_at_end() {
        let mut s = make_persona_state(three_personas(), "", 2);
        s.persona_select_next();
        assert_eq!(s.persona_selected, 2, "should not wrap past last item");
    }
    #[test]
    fn persona_select_prev_retreats() {
        let mut s = make_persona_state(three_personas(), "", 2);
        s.persona_select_prev();
        assert_eq!(s.persona_selected, 1);
        s.persona_select_prev();
        assert_eq!(s.persona_selected, 0);
    }
    #[test]
    fn persona_select_prev_clamps_at_start() {
        let mut s = make_persona_state(three_personas(), "", 0);
        s.persona_select_prev();
        assert_eq!(s.persona_selected, 0, "should not wrap past first item");
    }
    #[test]
    fn persona_select_next_recovers_when_selected_is_filtered_out() {
        let mut s = make_persona_state(three_personas(), "gamma", 0);
        assert_eq!(s.filtered_persona_indices(), vec![2]);
        s.persona_select_next();
        assert_eq!(s.persona_selected, 2);
    }
    #[test]
    fn persona_select_prev_recovers_when_selected_is_filtered_out() {
        let mut s = make_persona_state(three_personas(), "alpha", 2);
        assert_eq!(s.filtered_persona_indices(), vec![0]);
        s.persona_select_prev();
        assert_eq!(s.persona_selected, 0);
    }
    #[test]
    fn persona_select_noop_on_empty_list() {
        let mut s = make_persona_state(vec![], "", 0);
        s.persona_select_next();
        assert_eq!(s.persona_selected, 0, "should remain 0 on empty list");
        s.persona_select_prev();
        assert_eq!(s.persona_selected, 0, "should remain 0 on empty list");
    }
    /// On the Agents tab both `/` and `i` (no modifiers) activate the shared
    /// search.
    #[test]
    fn agents_tab_slash_and_i_activate_search() {
        for code in [KeyCode::Char('/'), KeyCode::Char('i')] {
            let mut s = make_persona_state(vec![], "", 0);
            s.active_tab = AgentsTab::Agents;
            assert!(!s.search_active);
            assert!(matches!(
                handle_agents_tab_key(&mut s, &KeyEvent::new(code, KeyModifiers::NONE)),
                AgentsModalOutcome::Changed
            ));
            assert!(s.search_active, "{code:?} must activate Agents-tab search");
        }
    }
    /// Personas symmetry: both `/` and `i` activate the shared search (the
    /// Personas tab now answers `/` too, matching the Agents tab).
    #[test]
    fn personas_tab_slash_and_i_activate_search() {
        for code in [KeyCode::Char('/'), KeyCode::Char('i')] {
            let mut s = make_persona_state(three_personas(), "", 0);
            assert!(!s.search_active);
            assert!(matches!(
                handle_personas_tab_key(&mut s, &KeyEvent::new(code, KeyModifiers::NONE)),
                AgentsModalOutcome::Changed
            ));
            assert!(
                s.search_active,
                "{code:?} must activate Personas-tab search"
            );
        }
    }
    /// The `modifiers.is_empty()` guard: Ctrl+i / Alt+i must NOT activate
    /// search on either tab.
    #[test]
    fn modified_i_does_not_activate_search_either_tab() {
        for mods in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let mut agents = make_persona_state(vec![], "", 0);
            agents.active_tab = AgentsTab::Agents;
            assert!(matches!(
                handle_agents_tab_key(&mut agents, &KeyEvent::new(KeyCode::Char('i'), mods)),
                AgentsModalOutcome::Unchanged
            ));
            assert!(!agents.search_active);
            let mut personas = make_persona_state(three_personas(), "", 0);
            assert!(matches!(
                handle_personas_tab_key(&mut personas, &KeyEvent::new(KeyCode::Char('i'), mods)),
                AgentsModalOutcome::Unchanged
            ));
            assert!(!personas.search_active);
        }
    }
    /// End-to-end: `i` survives the public dispatcher + chrome to reach the
    /// per-tab handler and activate search.
    #[test]
    fn handle_agents_key_i_activates_search_end_to_end() {
        let mut s = make_persona_state(vec![], "", 0);
        s.active_tab = AgentsTab::Agents;
        assert!(!s.search_active);
        assert!(matches!(
            handle_agents_key(
                &mut s,
                &KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)
            ),
            AgentsModalOutcome::Changed
        ));
        assert!(
            s.search_active,
            "`i` must survive chrome dispatch to activate search"
        );
    }
    /// Wiring check: both tab footers carry the shared `i search` hint under vim
    /// nav mode, and the Personas footer advertises `/ search` (symmetric with
    /// the Agents tab). The gate is covered centrally by `modal_window`'s
    /// `vim_nav_search_hint_only_in_vim_nav_mode`. The explicit `set_vim_mode`
    /// pin (a thread-local that, once set, blocks disk-seeding) keeps this
    /// independent of the dev's on-disk `[ui].vim_mode`; reset afterward since
    /// libtest reuses worker threads.
    #[test]
    fn tab_footers_advertise_i_search_under_vim() {
        crate::appearance::cache::set_vim_mode(true);
        let s = make_persona_state(three_personas(), "", 0);
        assert!(
            build_agents_tab_shortcuts(&s)
                .iter()
                .any(|sc| sc.label == "i search"),
            "vim-mode Agents footer must advertise `i search`"
        );
        assert!(
            build_personas_tab_shortcuts(&s)
                .iter()
                .any(|sc| sc.label == "i search"),
            "vim-mode Personas footer must advertise `i search`"
        );
        assert!(
            build_personas_tab_shortcuts(&s)
                .iter()
                .any(|sc| sc.label == "/ search"),
            "Personas browse footer must advertise `/ search`"
        );
        crate::appearance::cache::set_vim_mode(false);
    }
    #[test]
    fn search_text_changes_refilter_but_cursor_moves_do_not() {
        let mut state = make_persona_state(three_personas(), "", 0);
        state.search_active = true;
        let outcome = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert!(matches!(outcome, AgentsModalOutcome::Changed));
        assert_eq!(state.search_query(), "g");
        assert_eq!(state.persona_selected, 2);
        let outcome = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, AgentsModalOutcome::Changed));
        assert_eq!(state.search_query(), "g");
        assert_eq!(state.search_cursor_byte(), 0);
        assert_eq!(state.persona_selected, 2);
    }
    #[test]
    fn no_form_search_paste_sanitizes_at_cursor_and_resets_selection() {
        let mut state = make_persona_state(three_personas(), "ab", 2);
        state.search_active = true;
        state.message = Some(AgentsModalMessage::error("stale"));
        let _ = state.set_search_cursor_byte(1);
        let outcome = handle_agents_paste(&mut state, "中\r\n");
        assert!(matches!(outcome, AgentsModalOutcome::Changed));
        assert_eq!(state.search_query(), "a中b");
        assert_eq!(state.persona_selected, 2);
        assert!(state.filtered_persona_indices().is_empty());
        assert!(state.message.is_none());
        state.search_active = false;
        let outcome = handle_agents_paste(&mut state, "ignored");
        assert!(matches!(outcome, AgentsModalOutcome::Unchanged));
        assert_eq!(state.search_query(), "a中b");
    }
    #[test]
    fn create_text_field_paste_owns_input_and_clears_message_on_change() {
        let mut state = make_persona_state(three_personas(), "hidden", 1);
        state.search_active = true;
        state.persona_input = Some(PersonaCreateInput::new());
        state.message = Some(AgentsModalMessage::error("stale"));
        let outcome = handle_agents_paste(&mut state, "na\r\nme");
        assert!(matches!(outcome, AgentsModalOutcome::Changed));
        assert_eq!(
            state.persona_input.as_ref().map(PersonaCreateInput::name),
            Some("name")
        );
        assert_eq!(state.search_query(), "hidden");
        assert!(state.message.is_none());
    }
    #[test]
    fn scope_form_paste_is_consumed_without_hidden_search_fallthrough() {
        let mut state = make_persona_state(three_personas(), "hidden", 1);
        state.search_active = true;
        let mut input = PersonaCreateInput::new();
        input.active_field = CreateField::Scope;
        state.persona_input = Some(input);
        state.message = Some(AgentsModalMessage::error("keep"));
        let outcome = handle_agents_paste(&mut state, "must not leak");
        assert!(matches!(outcome, AgentsModalOutcome::Unchanged));
        let input = state.persona_input.as_ref().unwrap();
        assert!(input.name().is_empty());
        assert!(input.description().is_empty());
        assert!(input.instructions().is_empty());
        assert_eq!(state.search_query(), "hidden");
        assert_eq!(
            state.message.as_ref().map(|message| message.text.as_str()),
            Some("keep")
        );
    }
    #[test]
    fn handled_empty_paste_preserves_messages_for_form_and_search() {
        let mut state = make_persona_state(three_personas(), "search", 1);
        state.search_active = true;
        state.persona_input = Some(PersonaCreateInput::new());
        state.message = Some(AgentsModalMessage::error("form error"));
        let outcome = handle_agents_paste(&mut state, "\r\n");
        assert!(matches!(outcome, AgentsModalOutcome::Changed));
        assert_eq!(
            state.message.as_ref().map(|message| message.text.as_str()),
            Some("form error")
        );
        assert!(state.persona_input.as_ref().unwrap().name().is_empty());
        assert_eq!(state.search_query(), "search");
        state.persona_input = None;
        state.message = Some(AgentsModalMessage::error("search error"));
        let outcome = handle_agents_paste(&mut state, "\r\n");
        assert!(matches!(outcome, AgentsModalOutcome::Changed));
        assert_eq!(
            state.message.as_ref().map(|message| message.text.as_str()),
            Some("search error")
        );
        assert_eq!(state.search_query(), "search");
    }
    #[test]
    fn search_uses_canonical_word_and_grapheme_editing() {
        for key in [
            KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
        ] {
            let mut state = make_persona_state(three_personas(), "hello-world", 0);
            state.search_active = true;
            let outcome = handle_agents_key(&mut state, &key);
            assert!(matches!(outcome, AgentsModalOutcome::Changed));
            assert_eq!(state.search_query(), "hello-world");
            assert_eq!(state.search_cursor_byte(), "hello-".len());
        }
        for key in [
            KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
        ] {
            let mut state = make_persona_state(three_personas(), "hello-world", 0);
            state.search_active = true;
            let _ = state.set_search_cursor_byte(0);
            let outcome = handle_agents_key(&mut state, &key);
            assert!(matches!(outcome, AgentsModalOutcome::Changed));
            assert_eq!(state.search_query(), "hello-world");
            assert_eq!(state.search_cursor_byte(), "hello".len());
        }
        let grapheme = "👩🏽\u{200d}💻";
        let mut state = make_persona_state(three_personas(), &format!("a{grapheme}b"), 0);
        state.search_active = true;
        let _ = state.set_search_cursor_byte(1);
        let outcome = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, AgentsModalOutcome::Changed));
        assert_eq!(state.search_query(), "ab");
        assert_eq!(state.search_cursor_byte(), 1);
    }
    #[test]
    fn persona_create_field_navigation_keeps_jk_as_text() {
        let mut state = make_persona_state(three_personas(), "", 0);
        let _ = handle_personas_tab_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        for ch in ['j', 'k'] {
            let _ = handle_agents_key(
                &mut state,
                &KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        let input = state.persona_input.as_ref().unwrap();
        assert_eq!(input.name(), "jk");
        assert_eq!(input.active_field(), CreateField::Name);
        let outcome = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL),
        );
        assert!(matches!(outcome, AgentsModalOutcome::Unchanged));
        assert_eq!(
            state.persona_input.as_ref().unwrap().active_field(),
            CreateField::Name
        );
        let _ = handle_agents_key(&mut state, &KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            state.persona_input.as_ref().unwrap().active_field(),
            CreateField::Description
        );
        let _ = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
        );
        assert_eq!(
            state.persona_input.as_ref().unwrap().active_field(),
            CreateField::Name
        );
        let _ = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        assert_eq!(
            state.persona_input.as_ref().unwrap().active_field(),
            CreateField::Description
        );
        let _ = handle_agents_key(&mut state, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            state.persona_input.as_ref().unwrap().active_field(),
            CreateField::Name
        );
    }
    #[test]
    fn persona_create_validates_sanitizes_persists_and_rejects_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = make_persona_state(vec![], "", 0);
        state.cwd = directory.path().to_path_buf();
        let _ = handle_personas_tab_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        let outcome = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, AgentsModalOutcome::Changed));
        assert!(state.persona_input.is_some());
        assert_eq!(
            state.message.as_ref().map(|message| message.text.as_str()),
            Some("Name is required")
        );
        for ch in "my persona".chars() {
            let _ = handle_agents_key(
                &mut state,
                &KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        let _ = handle_agents_key(&mut state, &KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for ch in "helps".chars() {
            let _ = handle_agents_key(
                &mut state,
                &KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        let _ = handle_agents_key(&mut state, &KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for ch in "be useful".chars() {
            let _ = handle_agents_key(
                &mut state,
                &KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        let _ = handle_agents_key(&mut state, &KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let _ = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        assert_eq!(
            state.persona_input.as_ref().unwrap().scope(),
            ConfigFileScope::Project
        );
        let _ = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        let path = directory
            .path()
            .join(".echo-agent")
            .join("personas")
            .join("my-persona.toml");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("description = \"helps\""));
        assert!(content.contains("instructions = \"be useful\""));
        assert!(state.persona_input.is_none());
        let _ = handle_personas_tab_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        for ch in "my persona".chars() {
            let _ = handle_agents_key(
                &mut state,
                &KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        for _ in 0..3 {
            let _ = handle_agents_key(&mut state, &KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        let _ = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        let _ = handle_agents_key(
            &mut state,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(state.persona_input.is_some());
        assert!(
            state
                .message
                .as_ref()
                .is_some_and(|message| message.text.contains("already exists"))
        );
    }
    #[test]
    fn search_and_create_renderers_keep_unicode_cursor_visible() {
        let grapheme = "👩🏽\u{200d}💻";
        let text = format!("12345678901234567890中e\u{301}{grapheme}z");
        let theme = Theme::current();
        let mut state = make_persona_state(three_personas(), &text, 0);
        state.search_active = true;
        let _ = state.set_search_cursor_byte(text.len() - 1);
        let search_area = Rect::new(0, 0, 18, 1);
        let mut search_buffer = Buffer::empty(search_area);
        render_agents_search(
            &mut search_buffer,
            search_area,
            state.search_editor(),
            true,
            &theme,
        );
        let search_view = state.search_viewport(16);
        let search_visible = &state.search_query()[search_view.visible_byte_range.clone()];
        assert!(search_visible.contains('中'));
        assert!(search_visible.contains("e\u{301}"));
        assert!(search_visible.contains(grapheme));
        let search_cursor_x = 2 + search_view.cursor_display_column as u16;
        assert_eq!(search_buffer[(search_cursor_x, 0)].bg, theme.text_primary);
        state.search_active = false;
        let mut unfocused_search = Buffer::empty(search_area);
        render_agents_search(
            &mut unfocused_search,
            search_area,
            state.search_editor(),
            false,
            &theme,
        );
        let unfocused_text = (2..search_area.width)
            .map(|x| unfocused_search[(x, 0)].symbol())
            .collect::<String>();
        assert!(unfocused_text.starts_with("1234567890"));
        let mut input = PersonaCreateInput::new();
        input.set_field_text(CreateField::Name, &text);
        let _ = input.set_field_cursor_byte(CreateField::Name, text.len() - 1);
        let create_area = Rect::new(0, 0, 24, 12);
        let mut create_buffer = Buffer::empty(create_area);
        render_persona_create_form(&mut create_buffer, &create_area, &input, None, &theme);
        let editor_width = create_area.width as usize - "Name: ".len();
        let create_view = input.name_editor().viewport(editor_width);
        let create_visible = &input.name()[create_view.visible_byte_range.clone()];
        assert!(create_visible.contains('中'));
        assert!(create_visible.contains("e\u{301}"));
        assert!(create_visible.contains(grapheme));
        let create_cursor_x = "Name: ".len() as u16 + create_view.cursor_display_column as u16;
        assert_eq!(create_buffer[(create_cursor_x, 2)].bg, theme.text_primary);
        input.set_field_text(CreateField::Description, &text);
        let _ = input.set_field_cursor_byte(CreateField::Description, text.len() - 1);
        let mut inactive_buffer = Buffer::empty(create_area);
        render_persona_create_form(&mut inactive_buffer, &create_area, &input, None, &theme);
        let description_text = ("Description: ".len() as u16..create_area.width)
            .map(|x| inactive_buffer[(x, 4)].symbol())
            .collect::<String>();
        assert!(description_text.starts_with("1234567890"));
    }
    /// Fixture: a one-plugin registry whose `agents/` dir holds `reviewer.md`.
    fn plugin_registry_with_reviewer(
        plugin_root: &Path,
    ) -> echo_agent_core::plugins::PluginRegistry {
        use echo_agent_core::plugins::discovery::PluginId;
        use echo_agent_core::plugins::{
            DiscoveredPlugin, PluginManifest, PluginOrigin, PluginRegistry, PluginScope,
        };
        let agents_dir = plugin_root.join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews code\n---\nBody.\n",
        )
        .unwrap();
        let dp = DiscoveredPlugin {
            manifest: PluginManifest {
                name: "my-plugin".to_string(),
                ..Default::default()
            },
            id: PluginId::new(PluginScope::User, plugin_root, "my-plugin"),
            root: plugin_root.to_path_buf(),
            canonical_root: plugin_root.to_path_buf(),
            scope: PluginScope::User,
            origin: PluginOrigin::UserEchoAgent,
            trusted: true,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![agents_dir],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            conflict: None,
        };
        PluginRegistry::from_discovered(vec![dp], &[], &["my-plugin".to_string()])
    }
    #[test]
    fn build_agent_list_includes_plugin_agents_under_qualified_names() {
        let plugin_root = tempfile::tempdir().unwrap();
        let registry = plugin_registry_with_reviewer(plugin_root.path());
        let cwd = tempfile::tempdir().unwrap();
        let entries = build_agent_list(cwd.path(), &HashMap::new(), Some(&registry));
        let entry = entries
            .iter()
            .find(|e| e.name == "my-plugin:reviewer")
            .expect("plugin agent must be listed under its qualified name");
        assert_eq!(entry.description, "Reviews code");
        assert!(entry.enabled);
        assert!(!entry.is_builtin);
        assert!(
            entry.source_path.is_some(),
            "source path opens the .md file"
        );
        assert_eq!(entry.definition.plugin_name.as_deref(), Some("my-plugin"));
    }
    #[test]
    fn build_agent_list_plugin_agent_toggle_keys_on_qualified_name() {
        let plugin_root = tempfile::tempdir().unwrap();
        let registry = plugin_registry_with_reviewer(plugin_root.path());
        let cwd = tempfile::tempdir().unwrap();
        let toggle = HashMap::from([("my-plugin:reviewer".to_string(), false)]);
        let entries = build_agent_list(cwd.path(), &toggle, Some(&registry));
        let entry = entries
            .iter()
            .find(|e| e.name == "my-plugin:reviewer")
            .expect("disabled plugin agent stays visible in the list");
        assert!(!entry.enabled);
    }
}
