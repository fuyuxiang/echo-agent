//! Extensions modal popup (Hooks, Plugins, Marketplace, Skills, Workflows,
//! MCP Servers).
//!
//! A centered overlay using the shared [`ModalWindow`](super::modal_window)
//! chrome, opened by the `/hooks` and `/plugins` slash commands.
//! Blocks all input until closed with `Esc`.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::theme::Theme;
use crate::views::modal_window::{
    self, ModalContentArea, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};
use crate::views::picker;
use echo_agent_tools::implementations::skills::types::SkillInfo;

mod workflows_picker_rows;
use workflows_picker_rows::build_workflows_picker_rows;

/// Check if a name fuzzy-matches the search query.
/// Empty query matches everything.
fn fuzzy_matches(name: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query_lower = query.to_lowercase();
    let name_lower = name.to_lowercase();
    // Substring match first (fast path).
    if name_lower.contains(&query_lower) {
        return true;
    }
    // Fuzzy: all query chars appear in order in the name.
    let mut chars = query_lower.chars();
    let mut current = chars.next();
    for c in name_lower.chars() {
        if current == Some(c) {
            current = chars.next();
        }
    }
    current.is_none()
}

/// Check if a hook fuzzy-matches the search query across all its fields.
pub fn fuzzy_matches_hook(hook: &echo_agent_hooks_plugins_types::HookInfo, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    fuzzy_matches(&hook.name, query)
        || fuzzy_matches(&hook.event.to_string(), query)
        || hook
            .matcher
            .as_ref()
            .is_some_and(|m| fuzzy_matches(m, query))
        || hook
            .command
            .as_ref()
            .is_some_and(|c| fuzzy_matches(c, query))
        || hook.url.as_ref().is_some_and(|u| fuzzy_matches(u, query))
}

/// Case-insensitive string order without intermediate allocations.
/// Equal ignoring case falls back to the original byte order for stability.
fn cmp_str_ci(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().flat_map(char::to_lowercase);
    let mut bi = b.chars().flat_map(char::to_lowercase);
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x != y => return x.cmp(&y),
            _ => {}
        }
    }
}

fn hook_row_label(hook: &echo_agent_hooks_plugins_types::HookInfo) -> String {
    let matcher = hook
        .matcher
        .as_deref()
        .map(|m| format!(" /{m}"))
        .unwrap_or_default();
    format!("on:{}{matcher}", hook.event)
}

/// Logical source kind for hook group rank (independent of display copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HookSourceKind {
    Project = 0,
    Global = 1,
    Claude = 2,
    Plugin = 3,
    Custom = 4,
}

/// Label plus kind for a hooks source directory.
#[derive(Debug, Clone)]
struct HookSourceMeta {
    label: String,
    kind: HookSourceKind,
}

impl HookSourceMeta {
    fn is_custom(&self) -> bool {
        self.kind == HookSourceKind::Custom
    }
}

/// Sort key for hook source groups: kind rank, then A–Z label, then path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HookGroupSortKey<'a> {
    kind: HookSourceKind,
    label_lower: String,
    source_dir: &'a str,
}

fn hook_group_sort_key<'a>(source_dir: &'a str, meta: &HookSourceMeta) -> HookGroupSortKey<'a> {
    HookGroupSortKey {
        kind: meta.kind,
        label_lower: meta.label.to_lowercase(),
        source_dir,
    }
}

fn is_official_marketplace_source(
    source: &echo_agent_hooks_plugins_types::MarketplaceScanResult,
) -> bool {
    source.source_name == echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME
        || echo_agent_plugin_marketplace::is_official_source_url(&source.source_url_or_path)
}

/// One marketplace source in display order with plugins sorted A–Z.
#[derive(Debug, Clone)]
pub(crate) struct MarketplaceSourceView {
    pub source_index: usize,
    pub plugin_indices: Vec<usize>,
}

/// Official source first, then A–Z by `source_name`; plugins A–Z within each.
pub(crate) fn ordered_marketplace_view(
    sources: &[echo_agent_hooks_plugins_types::MarketplaceScanResult],
) -> Vec<MarketplaceSourceView> {
    let mut source_order: Vec<usize> = (0..sources.len()).collect();
    let source_keys: Vec<(bool, String)> = sources
        .iter()
        .map(|s| {
            (
                is_official_marketplace_source(s),
                s.source_name.to_lowercase(),
            )
        })
        .collect();
    source_order.sort_by(|&a, &b| match (source_keys[a].0, source_keys[b].0) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => source_keys[a]
            .1
            .cmp(&source_keys[b].1)
            .then_with(|| a.cmp(&b)),
    });
    source_order
        .into_iter()
        .map(|si| {
            let names: Vec<String> = sources[si]
                .plugins
                .iter()
                .map(|p| p.name.to_lowercase())
                .collect();
            let mut plugin_order: Vec<usize> = (0..sources[si].plugins.len()).collect();
            plugin_order.sort_by(|&a, &b| names[a].cmp(&names[b]).then_with(|| a.cmp(&b)));
            MarketplaceSourceView {
                source_index: si,
                plugin_indices: plugin_order,
            }
        })
        .collect()
}

/// Group header for a skill scope/source.
#[derive(Debug, Clone)]
struct SkillGroup {
    rank: u8,
    label: String,
}

/// Project → User → Plugin → Bundled → Server → Config.
fn skill_group(skill: &SkillInfo) -> SkillGroup {
    use echo_agent_tools::implementations::skills::types::SkillScope;
    use echo_agent_tools::types::config_source::ConfigSource;

    if let Some(ref cs) = skill.config_source {
        return match cs {
            ConfigSource::Project { .. } => SkillGroup {
                rank: 0,
                label: "Project".into(),
            },
            ConfigSource::User { .. } => SkillGroup {
                rank: 1,
                label: "User".into(),
            },
            ConfigSource::Plugin { plugin_name, .. } => SkillGroup {
                rank: 2,
                label: format!("Plugin: {plugin_name}"),
            },
            ConfigSource::Bundled { .. } => SkillGroup {
                rank: 3,
                label: "Bundled".into(),
            },
            ConfigSource::Server { .. } => SkillGroup {
                rank: 4,
                label: "Server".into(),
            },
            ConfigSource::Builtin
            | ConfigSource::ConfigToml { .. }
            | ConfigSource::ClaudeJson { .. }
            | ConfigSource::McpJson { .. }
            | ConfigSource::Cli { .. }
            | ConfigSource::Managed { .. } => SkillGroup {
                rank: 5,
                label: "Config".into(),
            },
        };
    }
    match skill.scope {
        SkillScope::Local | SkillScope::Repo => SkillGroup {
            rank: 0,
            label: "Project".into(),
        },
        SkillScope::User => SkillGroup {
            rank: 1,
            label: "User".into(),
        },
        SkillScope::Plugin => SkillGroup {
            rank: 2,
            label: format!(
                "Plugin: {}",
                skill.plugin_name.as_deref().unwrap_or("unknown")
            ),
        },
        SkillScope::Bundled => SkillGroup {
            rank: 3,
            label: "Bundled".into(),
        },
        SkillScope::Server => SkillGroup {
            rank: 4,
            label: "Server".into(),
        },
    }
}

/// Word-wrap text into lines that fit within `max_w` characters.
///
/// Splits on newlines first, then wraps each paragraph at word boundaries.
/// All slicing uses `char_indices` so multi-byte UTF-8 is never split.
fn word_wrap(text: &str, max_w: usize) -> Vec<&str> {
    if max_w == 0 {
        return vec![text];
    }
    let mut result = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            result.push(line);
            continue;
        }
        let mut remaining = line;
        while !remaining.is_empty() {
            if remaining.chars().count() <= max_w {
                result.push(remaining);
                break;
            }
            let byte_limit = remaining
                .char_indices()
                .nth(max_w)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len());
            let cut = remaining[..byte_limit]
                .rfind(' ')
                .filter(|&i| i > 0)
                .map(|i| i + 1)
                .unwrap_or(byte_limit);
            result.push(remaining[..cut].trim_end());
            remaining = remaining[cut..].trim_start();
        }
    }
    result
}

/// Test fixture: a minimal `PluginInfo` shared by the pager's plugin tests.
#[cfg(test)]
pub(crate) fn test_plugin_info(
    name: &str,
    origin: Option<echo_agent_hooks_plugins_types::PluginOrigin>,
) -> echo_agent_hooks_plugins_types::PluginInfo {
    echo_agent_hooks_plugins_types::PluginInfo {
        name: name.to_string(),
        id: format!("user/abcd1234/{name}"),
        root: format!("/tmp/{name}"),
        scope: echo_agent_hooks_plugins_types::PluginScope::User,
        trusted: true,
        enabled: true,
        version: None,
        description: None,
        skill_count: 0,
        skill_names: vec![],
        agent_count: 0,
        agent_names: vec![],
        hook_status: echo_agent_hooks_plugins_types::HookStatus::None,
        hook_count: 0,
        mcp_server_count: 0,
        mcp_status: echo_agent_hooks_plugins_types::McpStatus::None,
        marketplace_source: None,
        origin,
        conflict: None,
    }
}

/// A plugin's source group on the Plugins tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginGroup {
    /// Ordering bucket (lower renders first; groups with the same rank sort by label).
    pub rank: u8,
    /// Stable collapse key (stored in `entry_group_keys` and `plugins_collapsed_groups`).
    pub key: String,
    /// Header label shown for the group.
    pub label: String,
}

impl PluginGroup {
    fn new(rank: u8, key: &str, label: &str) -> Self {
        Self {
            rank,
            key: key.to_string(),
            label: label.to_string(),
        }
    }

    fn into_sort_key(self) -> PluginGroupSortKey {
        PluginGroupSortKey {
            rank: self.rank,
            label_lower: self.label.to_lowercase(),
            label: self.label,
            key: self.key,
        }
    }
}

/// Sort key for plugin origin groups (rank, then A–Z label).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PluginGroupSortKey {
    rank: u8,
    label_lower: String,
    label: String,
    key: String,
}

/// Plugins bucketed by group sort key for the Plugins tab.
type GroupedPlugins<'a> = std::collections::BTreeMap<
    PluginGroupSortKey,
    Vec<(usize, &'a echo_agent_hooks_plugins_types::PluginInfo)>,
>;

/// Header count suffix: `1 plugin`, `2 plugins`.
fn plugin_count_label(n: usize) -> String {
    if n == 1 {
        "1 plugin".to_string()
    } else {
        format!("{n} plugins")
    }
}

/// Resolve the source group a plugin belongs to on the Plugins tab.
///
/// Uses the plugin's `origin` when present. A missing origin (older shell)
/// or an unrecognized variant (newer shell) falls back to the scope plus
/// the legacy `marketplace_source` label so the UI still degrades to
/// sensible groups.
pub fn plugin_group(plugin: &echo_agent_hooks_plugins_types::PluginInfo) -> PluginGroup {
    use echo_agent_hooks_plugins_types::{PluginOrigin, PluginScope};

    match &plugin.origin {
        Some(PluginOrigin::ProjectEchoAgent) => PluginGroup::new(0, "origin:project", "Project"),
        Some(PluginOrigin::ProjectClaude) => {
            PluginGroup::new(1, "origin:project-claude", "Project (Claude)")
        }
        Some(PluginOrigin::UserEchoAgent) => PluginGroup::new(2, "origin:user", "User"),
        Some(PluginOrigin::UserClaude)
        | Some(PluginOrigin::ClaudeInstalled { marketplace: None }) => {
            PluginGroup::new(3, "origin:user-claude", "User (Claude)")
        }
        Some(PluginOrigin::ClaudeMarketplace { marketplace })
        | Some(PluginOrigin::ClaudeInstalled {
            marketplace: Some(marketplace),
        }) => PluginGroup {
            rank: 4,
            key: format!("claude-mp:{marketplace}"),
            label: marketplace.clone(),
        },
        Some(PluginOrigin::MarketplaceInstall {
            source_name: Some(source),
            ..
        }) => PluginGroup {
            rank: 5,
            key: format!("echo-agent-mp:{source}"),
            label: source.clone(),
        },
        Some(PluginOrigin::MarketplaceInstall {
            source_name: None, ..
        }) => PluginGroup::new(6, "origin:direct", "Direct installs"),
        Some(PluginOrigin::CliOverride) => PluginGroup::new(7, "origin:cli", "CLI override"),
        Some(PluginOrigin::ConfigPath) => PluginGroup::new(8, "origin:config", "Custom paths"),
        Some(PluginOrigin::Unknown) | None => match plugin.scope {
            PluginScope::Project => PluginGroup::new(0, "origin:project", "Project"),
            PluginScope::User => match plugin.marketplace_source.as_deref() {
                Some(source) if source.starts_with("git: ") => {
                    PluginGroup::new(6, "origin:direct", "Direct installs")
                }
                Some(source) => PluginGroup {
                    rank: 5,
                    key: format!("echo-agent-mp:{source}"),
                    label: source.to_string(),
                },
                None => PluginGroup::new(2, "origin:user", "User"),
            },
            PluginScope::Cli => PluginGroup::new(7, "origin:cli", "CLI override"),
            PluginScope::Config => PluginGroup::new(8, "origin:config", "Custom paths"),
        },
    }
}

/// Group + order hooks the same way the renderer does.
struct HookGroupView<'a> {
    source_dir: &'a str,
    label: String,
    indices: Vec<usize>,
}

fn build_hook_groups<'a>(
    hooks: &'a [echo_agent_hooks_plugins_types::HookInfo],
    filter: StatusFilter,
    query: &str,
) -> Vec<HookGroupView<'a>> {
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    for (i, hook) in hooks.iter().enumerate() {
        if !fuzzy_matches_hook(hook, query) {
            continue;
        }
        if !filter.matches(!hook.disabled) {
            continue;
        }
        if let Some(g) = groups.iter_mut().find(|g| g.0 == hook.source_dir) {
            g.1.push(i);
        } else {
            groups.push((&hook.source_dir, vec![i]));
        }
    }
    let mut metas: Vec<HookSourceMeta> = groups
        .iter()
        .map(|(dir, _)| classify_hook_source(dir))
        .collect();
    let group_keys: Vec<HookGroupSortKey<'_>> = groups
        .iter()
        .zip(metas.iter())
        .map(|((dir, _), meta)| hook_group_sort_key(dir, meta))
        .collect();
    let mut group_order: Vec<usize> = (0..groups.len()).collect();
    group_order.sort_by(|&a, &b| group_keys[a].cmp(&group_keys[b]));
    let mut ordered: Vec<HookGroupView<'a>> = Vec::with_capacity(groups.len());
    for gi in group_order {
        let (source_dir, mut indices) = std::mem::take(&mut groups[gi]);
        indices.sort_by_cached_key(|&i| (hook_row_label(&hooks[i]).to_lowercase(), i));
        ordered.push(HookGroupView {
            source_dir,
            label: std::mem::take(&mut metas[gi].label),
            indices,
        });
    }
    ordered
}

// ---------------------------------------------------------------------------
// Tab enum
// ---------------------------------------------------------------------------

/// Which tab is active in the hooks/plugins modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionsTab {
    Hooks,
    Plugins,
    Marketplace,
    Skills,
    Workflows,
    McpServers,
}

impl ExtensionsTab {
    /// All tabs in display order.
    pub const ALL: &[Self] = &[
        Self::Hooks,
        Self::Plugins,
        Self::Marketplace,
        Self::Skills,
        Self::Workflows,
        Self::McpServers,
    ];

    /// Display label for the tab bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hooks => "Hooks",
            Self::Plugins => "Plugins",
            Self::Marketplace => "Marketplace",
            Self::Skills => "Skills",
            Self::Workflows => "Workflows",
            Self::McpServers => "MCP Servers",
        }
    }

    /// Next tab (wraps around).
    pub fn next(self) -> Self {
        match self {
            Self::Hooks => Self::Plugins,
            Self::Plugins => Self::Marketplace,
            Self::Marketplace => Self::Skills,
            Self::Skills => Self::Workflows,
            Self::Workflows => Self::McpServers,
            Self::McpServers => Self::Hooks,
        }
    }
    /// Previous tab (wraps around).
    pub fn prev(self) -> Self {
        match self {
            Self::Hooks => Self::McpServers,
            Self::Plugins => Self::Hooks,
            Self::Marketplace => Self::Plugins,
            Self::Skills => Self::Marketplace,
            Self::Workflows => Self::Skills,
            Self::McpServers => Self::Workflows,
        }
    }

    pub fn telemetry_tab(self) -> echo_agent_telemetry::events::ExtensionsModalTab {
        use echo_agent_telemetry::events::ExtensionsModalTab;
        match self {
            Self::Hooks => ExtensionsModalTab::Hooks,
            Self::Plugins => ExtensionsModalTab::Plugins,
            Self::Marketplace => ExtensionsModalTab::Marketplace,
            Self::Skills => ExtensionsModalTab::Skills,
            Self::Workflows => ExtensionsModalTab::Workflows,
            Self::McpServers => ExtensionsModalTab::McpServers,
        }
    }
}

// ---------------------------------------------------------------------------
// Status filter
// ---------------------------------------------------------------------------

/// Filter items by enabled/disabled status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    All,
    Enabled,
    Disabled,
}

impl StatusFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Enabled => "Enabled",
            Self::Disabled => "Disabled",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Enabled,
            Self::Enabled => Self::Disabled,
            Self::Disabled => Self::All,
        }
    }

    pub fn matches(self, enabled: bool) -> bool {
        match self {
            Self::All => true,
            Self::Enabled => enabled,
            Self::Disabled => !enabled,
        }
    }
}

// ---------------------------------------------------------------------------
// Button actions
// ---------------------------------------------------------------------------

/// What a button does when activated (clicked or keyboard shortcut).
#[derive(Debug, Clone)]
pub enum ButtonAction {
    /// Execute a hooks action via ACP (no args needed).
    HooksAction(echo_agent_hooks_plugins_types::HooksAction),
    /// Execute a plugins action via ACP (no args needed).
    PluginsAction(echo_agent_hooks_plugins_types::PluginsAction),
    /// Remove the hook under the cursor (uses source_dir from selected hook).
    RemoveSelectedHook,
    /// Toggle enable/disable on the hook under the cursor.
    ToggleSelectedHook,
    /// Toggle enable/disable on the plugin under the cursor.
    ToggleSelectedPlugin,
    /// Toggle enable/disable on the skill under the cursor.
    ToggleSelectedSkill,
    /// Toggle enable/disable on the MCP server under the cursor.
    ToggleSelectedMcpServer,
    /// Trigger MCP auth for the selected server.
    McpAuthTrigger,
    /// Add an MCP server (parsed from inline input).
    AddMcpServer {
        name: String,
        config: Box<echo_agent_runtime::util::config::McpServerConfig>,
    },
    /// Remove the selected MCP server from config.toml.
    RemoveSelectedMcpServer,
    /// Reload the skills list (re-fetch from shell).
    ReloadSkills,
    /// Refresh MCP server list (re-fetch from shell).
    RefreshMcpList,
    /// Open cloud.echo-agent.invalid connectors page (MCP tab: press `o`).
    OpenManagedConnectors,
    /// Update (fetch latest from source) the selected plugin.
    UpdateSelectedPlugin,
    /// Uninstall the selected plugin.
    UninstallSelectedPlugin,
    /// Install the selected marketplace plugin.
    InstallSelectedMarketplacePlugin,
    /// Update the selected marketplace plugin.
    UpdateSelectedMarketplacePlugin,
    /// Uninstall the selected marketplace plugin.
    UninstallSelectedMarketplacePlugin,
    /// Execute a marketplace action via ACP.
    MarketplaceAction(echo_agent_hooks_plugins_types::MarketplaceAction),

    /// Remove the marketplace source under the cursor (unconfigure + uninstall all its plugins).
    RemoveSelectedMarketplaceSource,
    ToggleExpand,
    /// Cycle the status filter (All → Enabled → Disabled → All).
    CycleFilter,
    /// Enter input mode: show an inline form so the user can type arguments,
    /// then submit the full command on Enter.
    StartInput {
        /// Command prefix used to build the typed action on submit.
        command_prefix: String,
        /// Field specifications for the form (one per input field).
        fields: Vec<FieldSpec>,
    },
}

/// Specification for a single input field in the modal form.
#[derive(Debug, Clone)]
pub struct FieldSpec {
    /// Human-readable label shown before the input field (e.g., "Name").
    pub label: String,
    /// Whether the field must be non-empty to submit.
    pub required: bool,
    /// Placeholder text shown when the field is empty.
    pub placeholder: Option<String>,
}

/// Inline input state for commands that need one or more arguments.
#[derive(Debug, Clone)]
pub struct ModalInput {
    /// Command prefix used to build the typed action on submit.
    pub command_prefix: String,
    /// Input fields.
    fields: Vec<ModalInputField>,
    /// Index of the currently focused field.
    focused: usize,
    /// Inline error cleared by text edits, completion, or field navigation.
    pub error: Option<String>,
}

/// State for a single input field in the modal form.
#[derive(Debug, Clone)]
pub struct ModalInputField {
    /// Human-readable label shown before the input field.
    label: String,
    editor: LineEditor,
    /// Whether the field must be non-empty to submit.
    required: bool,
    /// Placeholder text shown when the field is empty.
    placeholder: Option<String>,
}

impl ModalInputField {
    fn new(spec: FieldSpec) -> Self {
        Self {
            label: spec.label,
            editor: LineEditor::default(),
            required: spec.required,
            placeholder: spec.placeholder,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn text(&self) -> &str {
        self.editor.text()
    }

    pub fn cursor_byte(&self) -> usize {
        self.editor.cursor_byte()
    }

    pub fn required(&self) -> bool {
        self.required
    }

    pub fn placeholder(&self) -> Option<&str> {
        self.placeholder.as_deref()
    }

    pub(crate) fn set_text(&mut self, text: impl Into<String>) {
        self.editor.set_text(text);
    }

    #[cfg(test)]
    fn set_cursor_byte(&mut self, cursor_byte: usize) -> LineEditOutcome {
        self.editor.set_cursor_byte(cursor_byte)
    }

    fn insert_paste(&mut self, text: &str) -> LineEditOutcome {
        self.editor.insert_paste(text)
    }

    fn handle_key(&mut self, key: &KeyEvent) -> LineEditOutcome {
        self.editor.handle_key(key)
    }

    pub(crate) fn viewport(&self, width: usize) -> echo_agent_ratatui_textarea::SingleLineViewport {
        self.editor.viewport(width)
    }
}

impl ModalInput {
    /// Build from a command prefix and field specs.
    pub fn from_specs(command_prefix: String, specs: Vec<FieldSpec>) -> Self {
        debug_assert!(!specs.is_empty(), "ModalInput needs at least one field");
        let fields = specs.into_iter().map(ModalInputField::new).collect();
        Self {
            command_prefix,
            fields,
            focused: 0,
            error: None,
        }
    }

    pub fn fields(&self) -> &[ModalInputField] {
        &self.fields
    }

    pub fn field(&self, index: usize) -> Option<&ModalInputField> {
        self.fields.get(index)
    }

    #[cfg(test)]
    fn field_mut(&mut self, index: usize) -> Option<&mut ModalInputField> {
        self.fields.get_mut(index)
    }

    pub fn focused_index(&self) -> usize {
        self.focused
    }

    fn focused_field_mut(&mut self) -> Option<&mut ModalInputField> {
        self.fields.get_mut(self.focused)
    }

    /// Whether there are multiple fields to navigate between.
    pub fn is_multi_field(&self) -> bool {
        self.fields.len() > 1
    }

    /// Collect all field texts into a Vec for submission.
    pub fn field_texts(&self) -> Vec<String> {
        self.fields
            .iter()
            .map(|field| field.text().to_owned())
            .collect()
    }

    /// Process a key event on the input form. Returns what the caller
    /// should do (submit, cancel, nothing, etc.) without coupling to
    /// `AgentView` or `InputOutcome`.
    pub fn handle_key(&mut self, key: &KeyEvent) -> ModalInputOutcome {
        match key {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => ModalInputOutcome::Cancel,

            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                let field_texts = self.field_texts();
                let empty_required: Vec<&str> = self
                    .fields
                    .iter()
                    .enumerate()
                    .filter(|(i, f)| {
                        f.required() && field_texts.get(*i).is_none_or(|t| t.trim().is_empty())
                    })
                    .map(|(_, f)| f.label())
                    .collect();
                if !empty_required.is_empty() {
                    self.error = Some(format!("Required: {}", empty_required.join(", ")));
                    return ModalInputOutcome::Changed;
                }
                ModalInputOutcome::Submit {
                    command_prefix: self.command_prefix.clone(),
                    field_texts,
                }
            }

            _ if crate::input::key::is_shift_tab(key) => {
                if !self.is_multi_field() {
                    return ModalInputOutcome::Unchanged;
                }
                self.error = None;
                self.focused = if self.focused == 0 {
                    self.fields.len() - 1
                } else {
                    self.focused - 1
                };
                ModalInputOutcome::Changed
            }
            _ if crate::input::key::KeyShortcut::key(KeyCode::Tab).matches(key) => {
                if self.is_multi_field() {
                    self.error = None;
                    self.focused = (self.focused + 1) % self.fields.len();
                    return ModalInputOutcome::Changed;
                }
                let completed = self.focused_field_mut().and_then(|field| {
                    let partial = field.text()[..field.cursor_byte()].to_owned();
                    tab_complete_path(&partial)
                });
                if let Some(completed) = completed {
                    if let Some(field) = self.focused_field_mut() {
                        field.set_text(completed);
                    }
                    self.error = None;
                    return ModalInputOutcome::Changed;
                }
                ModalInputOutcome::Unchanged
            }

            _ if crate::input::key::is_paste_key(key) => {
                let Some(clip) = crate::clipboard::system_clipboard_get() else {
                    return ModalInputOutcome::Unchanged;
                };
                let outcome = self
                    .focused_field_mut()
                    .map_or(LineEditOutcome::Unhandled, |field| {
                        field.insert_paste(&clip)
                    });
                if outcome == LineEditOutcome::TextChanged {
                    self.error = None;
                    ModalInputOutcome::Changed
                } else {
                    ModalInputOutcome::Unchanged
                }
            }

            _ => {
                let outcome = self
                    .focused_field_mut()
                    .map_or(LineEditOutcome::Unhandled, |field| field.handle_key(key));
                self.finish_line_edit(outcome)
            }
        }
    }

    fn finish_line_edit(&mut self, outcome: LineEditOutcome) -> ModalInputOutcome {
        match outcome {
            LineEditOutcome::TextChanged => {
                self.error = None;
                ModalInputOutcome::Changed
            }
            LineEditOutcome::HandledNoChange | LineEditOutcome::CursorChanged => {
                ModalInputOutcome::Changed
            }
            LineEditOutcome::Unhandled => ModalInputOutcome::Unchanged,
        }
    }

    fn insert_paste(&mut self, text: &str) -> bool {
        let outcome = self
            .focused_field_mut()
            .map_or(LineEditOutcome::Unhandled, |field| field.insert_paste(text));
        if outcome == LineEditOutcome::TextChanged {
            self.error = None;
            true
        } else {
            false
        }
    }
}

/// Result of processing a key event on the modal input form.
#[derive(Debug)]
pub enum ModalInputOutcome {
    /// Event was consumed and a redraw is needed.
    Changed,
    /// No state change, skip redraw.
    Unchanged,
    /// User pressed Esc, close the input form.
    Cancel,
    /// User pressed Enter and all required fields are filled.
    Submit {
        command_prefix: String,
        field_texts: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct McpSetupFormState {
    pub server_name: String,
    pub field: crate::views::mcps_modal::McpSetupField,
    pub selected: usize,
    pub error: Option<String>,
}

impl McpSetupFormState {
    pub fn new(server: &crate::views::mcps_modal::McpServerInfo) -> Option<Self> {
        let setup = server.setup.as_ref()?.clone();
        Self::from_setup(server.name.clone(), setup, server.setup_values.clone())
    }

    pub fn from_setup(
        server_name: String,
        setup: crate::views::mcps_modal::McpSetupConfig,
        values: std::collections::HashMap<String, String>,
    ) -> Option<Self> {
        if setup.fields.len() != 1 {
            return None;
        }
        let field = setup.fields.into_iter().next()?;
        if field.options.is_empty() {
            return None;
        }
        let selected = values
            .get(&field.id)
            .or(field.default.as_ref())
            .and_then(|value| {
                field
                    .options
                    .iter()
                    .position(|option| option.value == *value)
            })
            .unwrap_or(0);
        Some(Self {
            server_name,
            field,
            selected,
            error: None,
        })
    }

    pub fn selected_value(&self) -> Option<String> {
        self.field
            .options
            .get(self.selected)
            .map(|option| option.value.clone())
    }

    pub fn values(&self) -> Option<std::collections::HashMap<String, String>> {
        let mut values = std::collections::HashMap::new();
        values.insert(self.field.id.clone(), self.selected_value()?);
        Some(values)
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> McpSetupOutcome {
        match key.code {
            KeyCode::Esc => McpSetupOutcome::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.error = None;
                if self.selected > 0 {
                    self.selected -= 1;
                }
                McpSetupOutcome::Changed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.error = None;
                if self.selected + 1 < self.field.options.len() {
                    self.selected += 1;
                }
                McpSetupOutcome::Changed
            }
            KeyCode::Enter => {
                if self.selected_value().is_none() {
                    self.error = Some("Select an option".to_string());
                    McpSetupOutcome::Changed
                } else {
                    McpSetupOutcome::Submit
                }
            }
            _ => McpSetupOutcome::Unchanged,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSetupOutcome {
    Changed,
    Unchanged,
    Cancel,
    Submit,
}

/// Concrete action to run after the user presses `y` on a confirmation overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmationAction {
    /// Replay a hooks action (e.g. remove a hook source directory).
    Hooks(echo_agent_hooks_plugins_types::HooksAction),
    /// Replay a plugins action (e.g. uninstall; may still be `confirmed: false`
    /// so multi-plugin repos can return a second server-owned prompt).
    Plugins(echo_agent_hooks_plugins_types::PluginsAction),
    /// Replay a marketplace action (uninstall plugin or remove source).
    Marketplace(echo_agent_hooks_plugins_types::MarketplaceAction),
    /// Delete a removable (local) MCP server by name.
    DeleteMcpServer { server_name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalMessage {
    Error(String),
    Confirmation {
        message: String,
        action: ConfirmationAction,
        pending_entry_index: Option<usize>,
    },
}

/// A rendered button's hit area and associated action.
#[derive(Debug, Clone)]
pub struct ButtonArea {
    pub rect: Rect,
    pub action: ButtonAction,
    pub key: char,
}

/// Transient, non-covering feedback shown after an extensions action
/// completes, so the list (the surface that actually changed — a bumped
/// version, a refreshed list) stays visible. Auto-expires via a per-tick
/// countdown, mirroring `AgentView::toast`.
#[derive(Debug, Clone)]
pub struct ActionResultNotice {
    /// Full result text (used verbatim for the tab-wide status line).
    pub message: String,
    /// Row to anchor a badge to; `None` renders a tab-wide status line.
    pub entry_index: Option<usize>,
    /// Remaining animation ticks before auto-dismiss.
    pub ticks_remaining: u16,
}

/// How long a result notice stays on screen, in animation ticks (~2.5s at 30fps).
pub const RESULT_NOTICE_TICKS: u16 = 75;

/// Single source of truth for the McpServers tab action keys. Consumed by
/// the renderer (hint bar), the picker (`PickerConfig::action_keys`), and
/// `resolve_key` (must have a matching arm for every entry).
pub const MCP_SERVERS_ACTION_KEYS: &[(char, &str)] = &[
    ('r', "refresh"),
    ('a', "add"),
    ('i', "auth"),
    (' ', "toggle"),
    ('x', "remove"),
];

/// Footer label for the MCP tab Ctrl+O shortcut (not in [`MCP_SERVERS_ACTION_KEYS`]).
pub const MCP_SERVERS_OPEN_CONNECTORS_FOOTER: &str = "ctrl-o open";

/// Map an action key character to its display string for shortcut hints.
///
/// Single source of truth shared by [`render_extensions_modal`] (footer
/// shortcuts) and [`crate::views::picker::render_picker`] (hint bar).
/// Returns `""` for unmapped characters.
pub fn action_key_display(ch: char) -> &'static str {
    match ch {
        ' ' => "space",
        'a' => "a",
        'd' => "d",
        'e' => "e",
        'f' => "f",
        'i' => "i",
        'o' => "o",
        'r' => "r",
        'u' => "u",
        'x' => "x",
        _ => "",
    }
}

/// Per-tab action keys for the extensions modal (footer, picker, telemetry).
///
/// Space stays labeled `"toggle"` on the wire for telemetry / picker identity;
/// user-facing copy remaps via [`action_key_footer_desc`] /
/// [`action_key_cheatsheet_desc`].
pub fn extensions_action_keys(tab: ExtensionsTab) -> Vec<(char, &'static str)> {
    match tab {
        ExtensionsTab::Hooks => vec![
            ('r', "reload"),
            ('a', "add"),
            (' ', "toggle"),
            ('x', "remove"),
        ],
        ExtensionsTab::Plugins => vec![
            ('r', "reload"),
            ('u', "update"),
            ('a', "install"),
            (' ', "toggle"),
            ('x', "uninstall"),
        ],
        ExtensionsTab::Marketplace => vec![
            ('i', "install"),
            ('r', "refresh"),
            ('u', "update"),
            ('a', "add source"),
            ('d', "uninstall"),
            ('x', "remove source"),
        ],
        ExtensionsTab::Skills => vec![(' ', "toggle"), ('f', "filter"), ('r', "reload")],
        ExtensionsTab::Workflows => vec![('r', "reload")],
        ExtensionsTab::McpServers => MCP_SERVERS_ACTION_KEYS.to_vec(),
    }
}

/// Footer verb for an action key. Uses the state's published
/// `entry_data_indices` / `entry_group_keys` (input handling, unit tests).
pub fn action_key_footer_desc(
    ch: char,
    desc: &'static str,
    state: &ExtensionsModalState,
) -> &'static str {
    action_key_footer_desc_for_mapping(
        ch,
        desc,
        state,
        &state.entry_data_indices,
        &state.entry_group_keys,
        state.picker_state.selected,
    )
}

/// Like [`action_key_footer_desc`], but resolves the Space enable/disable verb
/// from freshly built entry-mapping slices (render path) so we need not
/// publish `state.entry_*` before paint.
fn action_key_footer_desc_for_mapping(
    ch: char,
    desc: &'static str,
    state: &ExtensionsModalState,
    entry_data_indices: &[Option<usize>],
    entry_group_keys: &[Option<String>],
    selected: usize,
) -> &'static str {
    if ch == ' ' && desc == "toggle" {
        match selected_item_enabled_at(state, entry_data_indices, entry_group_keys, selected) {
            Some(true) => "disable",
            Some(false) => "enable",
            None => "enable/disable",
        }
    } else {
        desc
    }
}

pub fn action_key_cheatsheet_desc(ch: char, desc: &'static str) -> &'static str {
    if ch == ' ' && desc == "toggle" {
        "enable/disable"
    } else {
        desc
    }
}

fn data_index_at(entry_data_indices: &[Option<usize>], selected: usize) -> Option<usize> {
    entry_data_indices.get(selected).copied().flatten()
}

/// Resolve picker selection to `(server_index, tool_index)` for an MCP tool row,
/// using the given entry-mapping slices (not necessarily yet published on `state`).
fn selected_mcp_tool_at(
    entry_data_indices: &[Option<usize>],
    entry_group_keys: &[Option<String>],
    selected: usize,
) -> Option<(usize, usize)> {
    if entry_group_keys.get(selected)?.is_some() {
        return None;
    }
    let parent_si = data_index_at(entry_data_indices, selected)?;
    let parent_pos = (0..selected).rev().find(|&i| {
        entry_group_keys
            .get(i)
            .and_then(|k| k.as_ref())
            .is_some_and(|k| k.starts_with("mcp-tools:"))
    })?;
    Some((parent_si, selected - parent_pos - 1))
}

/// Whether the selected row is enabled, using the given entry-mapping slices
/// so the render path can resolve Space enable/disable without an early
/// `state.entry_*` publish.
fn selected_item_enabled_at(
    state: &ExtensionsModalState,
    entry_data_indices: &[Option<usize>],
    entry_group_keys: &[Option<String>],
    selected: usize,
) -> Option<bool> {
    match state.active_tab {
        ExtensionsTab::Plugins => {
            let idx = data_index_at(entry_data_indices, selected)?;
            match &state.plugins_data {
                TabDataState::Loaded(data) => data.plugins.get(idx).map(|p| p.enabled),
                _ => None,
            }
        }
        ExtensionsTab::Hooks => {
            let idx = data_index_at(entry_data_indices, selected)?;
            match &state.hooks_data {
                TabDataState::Loaded(data) => {
                    let hook = data.hooks.get(idx)?;
                    if state.hooks_collapsed_groups.contains(&hook.source_dir) {
                        Some(
                            data.hooks
                                .iter()
                                .filter(|h| h.source_dir == hook.source_dir)
                                .any(|h| !h.disabled),
                        )
                    } else {
                        Some(!hook.disabled)
                    }
                }
                _ => None,
            }
        }
        ExtensionsTab::Skills => {
            let idx = data_index_at(entry_data_indices, selected)?;
            match &state.skills_data {
                TabDataState::Loaded(skills) => skills.get(idx).map(|s| s.enabled),
                _ => None,
            }
        }
        ExtensionsTab::McpServers => match &state.mcps_data {
            TabDataState::Loaded(servers) => {
                if let Some((si, ti)) =
                    selected_mcp_tool_at(entry_data_indices, entry_group_keys, selected)
                {
                    return servers
                        .get(si)
                        .and_then(|s| s.tools.get(ti))
                        .map(|t| t.enabled);
                }
                let idx = data_index_at(entry_data_indices, selected)?;
                servers.get(idx).map(|s| s.enabled)
            }
            _ => None,
        },
        ExtensionsTab::Marketplace => None,
        ExtensionsTab::Workflows => None,
    }
}

/// Build the full list of hint items for a given extensions tab (for the
/// current section to surface all tab action keys, not just the compact
/// subset shown in the bottom bar).
pub fn tab_all_hints(tab: ExtensionsTab) -> Vec<crate::views::shortcuts_bar::HintItem> {
    use crate::input::key::KeyShortcut;
    use crate::views::shortcuts_bar::HintItem;
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut hints: Vec<HintItem> = Vec::new();
    for (ch, label) in extensions_action_keys(tab) {
        let display_key = KeyShortcut::new(KeyCode::Char(ch), KeyModifiers::NONE);
        let mut item = HintItem::new(display_key, action_key_cheatsheet_desc(ch, label));
        if ch == ' ' {
            item.custom_display = Some("Space");
        }
        hints.push(item);
    }
    // Common navigation.
    hints.push(HintItem::paired(crate::key!('j'), crate::key!('k'), "nav"));
    hints.push(HintItem::new(crate::key!(Tab), "switch tab"));
    hints.push(HintItem::new(crate::key!('/'), "search"));
    hints.push(HintItem::new(crate::key!(Enter), "expand"));
    hints.push(HintItem::new(crate::key!(Esc), "close"));
    hints
}

pub fn action_telemetry_label(tab: ExtensionsTab, ch: char) -> Option<String> {
    extensions_action_keys(tab)
        .iter()
        .find(|&&(c, _)| c == ch)
        .map(|&(_, label)| label.replace(' ', "_"))
}

/// Resolve a key press to a button action based on the active tab.
pub fn resolve_key(tab: ExtensionsTab, ch: char) -> Option<ButtonAction> {
    use echo_agent_hooks_plugins_types::{HooksAction, MarketplaceAction, PluginsAction};

    match (tab, ch) {
        // Plugins tab
        (ExtensionsTab::Plugins, 'r') => Some(ButtonAction::PluginsAction(PluginsAction::Reload)),
        // Update (fetch latest from source) the selected installed plugin.
        (ExtensionsTab::Plugins, 'u') => Some(ButtonAction::UpdateSelectedPlugin),
        (ExtensionsTab::Marketplace, 'u') => Some(ButtonAction::UpdateSelectedMarketplacePlugin),
        (ExtensionsTab::Plugins, 'a') => Some(ButtonAction::StartInput {
            command_prefix: "plugins_install".into(),
            fields: vec![FieldSpec {
                label: "Source".into(),
                required: true,
                placeholder: Some("owner/repo, URL, or local path".into()),
            }],
        }),
        // Toggle enable/disable on the selected plugin.
        (ExtensionsTab::Plugins, ' ') => Some(ButtonAction::ToggleSelectedPlugin),
        (ExtensionsTab::Plugins, 'x') => Some(ButtonAction::UninstallSelectedPlugin),
        // Hooks tab
        (ExtensionsTab::Hooks, 'r') => Some(ButtonAction::HooksAction(HooksAction::Reload)),
        (ExtensionsTab::Hooks, 'a') => Some(ButtonAction::StartInput {
            command_prefix: "hooks_add".into(),
            fields: vec![FieldSpec {
                label: "Path".into(),
                required: true,
                placeholder: None,
            }],
        }),
        // Remove acts on the selected hook — resolved at dispatch time.
        (ExtensionsTab::Hooks, 'x') => Some(ButtonAction::RemoveSelectedHook),
        // Toggle enable/disable on the selected hook.
        (ExtensionsTab::Hooks, ' ') => Some(ButtonAction::ToggleSelectedHook),
        // Marketplace tab
        (ExtensionsTab::Marketplace, 'i') => Some(ButtonAction::InstallSelectedMarketplacePlugin),
        (ExtensionsTab::Marketplace, 'r') => Some(ButtonAction::MarketplaceAction(
            MarketplaceAction::Refresh {
                source_url_or_path: None,
            },
        )),
        (ExtensionsTab::Marketplace, 'd') => Some(ButtonAction::UninstallSelectedMarketplacePlugin),
        (ExtensionsTab::Marketplace, 'a') => Some(ButtonAction::StartInput {
            command_prefix: "marketplace_add_source".into(),
            fields: vec![FieldSpec {
                label: "Source".into(),
                required: true,
                placeholder: Some("owner/repo, git URL, or local path".into()),
            }],
        }),
        (ExtensionsTab::Marketplace, 'x') => Some(ButtonAction::RemoveSelectedMarketplaceSource),
        (ExtensionsTab::Skills, ' ') => Some(ButtonAction::ToggleSelectedSkill),
        (ExtensionsTab::Skills, 'r') => Some(ButtonAction::ReloadSkills),
        (ExtensionsTab::Skills, 'f') => Some(ButtonAction::CycleFilter),
        // Shares ReloadSkills: its router arm refetches skills and workflows.
        (ExtensionsTab::Workflows, 'r') => Some(ButtonAction::ReloadSkills),
        (ExtensionsTab::McpServers, 'a') => Some(ButtonAction::StartInput {
            command_prefix: "mcp_add".into(),
            // URL is required, Name is optional (auto-derived from URL),
            // so URL goes first to match the natural typing order.
            // `build_action_from_input` reads matching indices.
            fields: vec![
                FieldSpec {
                    label: "URL / Command".into(),
                    required: true,
                    placeholder: Some("https://... or command [args...]".into()),
                },
                FieldSpec {
                    label: "Name".into(),
                    required: false,
                    placeholder: Some("Auto generated by URL".into()),
                },
            ],
        }),
        (ExtensionsTab::McpServers, 'x') => Some(ButtonAction::RemoveSelectedMcpServer),
        (ExtensionsTab::McpServers, 'r') => Some(ButtonAction::RefreshMcpList),
        (ExtensionsTab::McpServers, ' ') => Some(ButtonAction::ToggleSelectedMcpServer),
        (ExtensionsTab::McpServers, 'i') => Some(ButtonAction::McpAuthTrigger),
        (ExtensionsTab::Hooks, 'f') => Some(ButtonAction::CycleFilter),
        (ExtensionsTab::Plugins, 'f') => Some(ButtonAction::CycleFilter),
        (ExtensionsTab::McpServers, 'f') => Some(ButtonAction::CycleFilter),
        _ => None,
    }
}

/// Tab-complete a partial path by listing directory entries.
///
/// Expands `~` to home directory. If the partial path is a directory,
/// lists its contents. If it's a partial filename, finds matching entries
/// in the parent directory. Returns the longest common prefix among matches,
/// or `None` if no matches or the path doesn't exist.
pub fn tab_complete_path(partial: &str) -> Option<String> {
    use std::path::Path;

    if partial.is_empty() {
        return None;
    }

    // Expand ~ to home directory.
    let expanded = if let Some(rest) = partial.strip_prefix('~') {
        let home = dirs::home_dir()?;
        if rest.is_empty() || rest == "/" {
            home.to_string_lossy().to_string() + "/"
        } else {
            home.join(rest.strip_prefix('/').unwrap_or(rest))
                .to_string_lossy()
                .to_string()
        }
    } else {
        partial.to_string()
    };

    let path = Path::new(&expanded);

    // If path is an existing directory (ends with /), list its contents.
    if path.is_dir() && expanded.ends_with('/') {
        let mut entries: Vec<String> = std::fs::read_dir(path)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let full = path.join(&name);
                if full.is_dir() {
                    format!("{expanded}{name}/")
                } else {
                    format!("{expanded}{name}")
                }
            })
            .collect();
        entries.sort();
        if entries.len() == 1 {
            return Some(entries.into_iter().next().unwrap());
        }
        if entries.len() > 1 {
            return Some(longest_common_prefix(&entries));
        }
        return None;
    }

    // Partial filename: complete in parent directory.
    let parent = path.parent()?;
    let prefix = path.file_name()?.to_string_lossy().to_string();

    if !parent.is_dir() {
        return None;
    }

    let mut matches: Vec<String> = std::fs::read_dir(parent)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&prefix) && !name.starts_with('.')
        })
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let full = parent.join(&name);
            let parent_str = if expanded.contains('/') {
                expanded
                    .rsplit_once('/')
                    .map(|(p, _)| format!("{p}/"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if full.is_dir() {
                format!("{parent_str}{name}/")
            } else {
                format!("{parent_str}{name}")
            }
        })
        .collect();

    matches.sort();
    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some(matches.into_iter().next().unwrap());
    }
    Some(longest_common_prefix(&matches))
}

/// Find the longest common prefix among a sorted list of strings.
fn longest_common_prefix(strings: &[String]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = &strings[0];
    let last = &strings[strings.len() - 1];
    first
        .chars()
        .zip(last.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a)
        .collect()
}
/// Collect characters from `text` until the accumulated display width
/// reaches `max_w`. Prevents wide characters (CJK, emoji) from
/// overflowing a fixed-width column.
fn take_by_width(text: &str, max_w: usize) -> String {
    let mut w = 0;
    text.chars()
        .take_while(|c| {
            let cw = unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            if w + cw > max_w {
                return false;
            }
            w += cw;
            true
        })
        .collect()
}

/// Build a typed action from a multi-field form submission.
///
/// `field_texts` contains one entry per field in submission order.
/// Single-field forms pass a 1-element slice; MCP add passes
/// `[url_or_command, name]` — URL first to match the on-screen field
/// order (URL is required, Name is optional / auto-derived).
pub fn build_action_from_input(
    command_prefix: &str,
    field_texts: &[String],
) -> Option<ButtonAction> {
    use echo_agent_hooks_plugins_types::{HooksAction, PluginsAction};

    let first = field_texts.first().map(|s| s.trim()).unwrap_or("");

    match command_prefix {
        "plugins_install" => Some(ButtonAction::PluginsAction(PluginsAction::Install {
            source: first.to_string(),
        })),
        "plugins_uninstall" => Some(ButtonAction::PluginsAction(PluginsAction::Uninstall {
            plugin_id: first.to_string(),
            confirmed: false,
        })),
        "hooks_add" => Some(ButtonAction::HooksAction(HooksAction::Add {
            path: first.to_string(),
        })),
        "hooks_remove" => Some(ButtonAction::HooksAction(HooksAction::Remove {
            path: first.to_string(),
        })),
        "marketplace_add_source" => Some(ButtonAction::MarketplaceAction(
            echo_agent_hooks_plugins_types::MarketplaceAction::AddSource {
                url: first.to_string(),
            },
        )),
        "mcp_add" => {
            // Field order: [URL / Command, Name]. URL is required.
            let url_or_cmd = first.to_string();
            let name = field_texts
                .get(1)
                .map(|s| s.trim())
                .unwrap_or_default()
                .to_string();
            if url_or_cmd.is_empty() {
                return None;
            }
            parse_mcp_add_fields(&name, &url_or_cmd)
        }
        _ => None,
    }
}

/// Derive a server name from a URL by extracting a meaningful hostname segment.
///
/// `https://mcp.linear.app/mcp` -> `linear`
/// `https://example.com/mcp` -> `example`
fn derive_name_from_url(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("server")
        .split('.')
        .find(|seg| !seg.is_empty() && *seg != "mcp" && *seg != "www")
        .unwrap_or("server")
        .to_string()
}

/// Parse MCP add from separate name and url/command fields.
///
/// If `name` is empty, derives a name from the URL hostname.
/// The `url_or_cmd` field is split on whitespace to extract the command
/// and any trailing args for stdio transport.
fn parse_mcp_add_fields(name: &str, url_or_cmd: &str) -> Option<ButtonAction> {
    use echo_agent_runtime::util::config::{McpServerConfig, McpServerTransportConfig};

    let mut parts = url_or_cmd.split_whitespace();
    let command_or_url = parts.next()?;
    let rest: Vec<String> = parts.map(String::from).collect();

    let is_url = command_or_url.starts_with("http://") || command_or_url.starts_with("https://");

    let name = if name.is_empty() {
        if is_url {
            derive_name_from_url(command_or_url)
        } else {
            command_or_url.to_string()
        }
    } else {
        name.to_string()
    };

    let transport = if is_url {
        McpServerTransportConfig::StreamableHttp {
            url: command_or_url.to_string(),
            transport_type: None,
            bearer_token_env_var: None,
            headers: None,
            oauth_client_id: None,
            oauth_client_secret_env_var: None,
            oauth_scopes: None,
        }
    } else {
        McpServerTransportConfig::Stdio {
            command: command_or_url.to_string(),
            args: rest,
            env: None,
            cwd: None,
        }
    };

    Some(ButtonAction::AddMcpServer {
        name,
        config: Box::new(McpServerConfig {
            desktop_instance_id: None,
            transport,
            enabled: true,
            oauth: None,
            setup: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            tool_timeouts: None,
            expose_image_base64: None,
        }),
    })
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Per-tab data fetching lifecycle.
#[derive(Debug)]
pub enum TabDataState<T> {
    /// Fetch in progress (or not yet started).
    Loading,
    /// Data loaded successfully.
    Loaded(T),
    /// Fetch failed.
    Error(String),
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct WorkflowInfo {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub source: String,
    pub path: Option<String>,
}

/// State for the hooks/plugins modal popup.
pub struct ExtensionsModalState {
    /// Shared modal window chrome state (close button, tabs, footer
    /// shortcuts, popup area). Replaces the former `last_popup_area`,
    /// `tab_areas`, `close_button_area`, `close_hovered` fields.
    pub window: ModalWindowState,
    /// Currently active tab (source of truth).
    ///
    /// `window.active_tab` (a `usize` index) is derived from this in the
    /// render path via `ExtensionsTab::ALL.position()`. Only this field
    /// should be mutated by input handlers; the window's copy is a
    /// rendering hint synced each frame.
    pub active_tab: ExtensionsTab,
    /// Session team principal for managed-connectors deep links in section copy.
    pub session_team_id: Option<String>,
    /// Hooks list data (fetched from shell).
    pub hooks_data: TabDataState<echo_agent_hooks_plugins_types::HooksListResponse>,
    /// Plugins list data (fetched from shell).
    pub plugins_data: TabDataState<echo_agent_hooks_plugins_types::PluginsListResponse>,
    /// Cached button hit areas from last render (for mouse click).
    pub button_areas: Vec<ButtonArea>,
    /// Active inline input (when the user is typing an argument for a command).
    /// `None` = normal button mode, `Some` = input mode.
    pub input: Option<ModalInput>,
    pub mcp_setup: Option<McpSetupFormState>,
    /// Modal message state (error, confirmation prompt, etc.).
    pub modal_message: Option<ModalMessage>,
    /// Description of an in-flight action (blocks buttons while set).
    pub pending_action: Option<String>,
    /// Picker entry index with an in-flight action (shown as inline badge).
    /// Not invalidated on entry-list changes (filter/refresh/tab-switch);
    /// out-of-range is skipped at render time, but a stale-but-in-range
    /// index can decorate the wrong row.
    pub pending_entry_index: Option<usize>,
    /// Transient result feedback shown after an action succeeds: a right-aligned
    /// badge on `entry_index`'s row, or a tab-wide footer line when `None`.
    pub result_notice: Option<ActionResultNotice>,
    /// Last dispatched plugins action (for confirmation replay).
    pub last_plugins_action: Option<echo_agent_hooks_plugins_types::PluginsAction>,
    /// Selected item index per tab (for j/k navigation).
    /// Maps visible row offset (relative to content top) to hook index.
    /// Rebuilt every render; used for mouse click → hook selection.
    pub hooks_visible_map: Vec<Option<usize>>,
    pub hooks_selected: usize,
    pub plugins_selected: usize,
    /// Scroll offset per tab.
    pub hooks_scroll: usize,
    pub plugins_scroll: usize,
    /// Marketplace tab state.
    pub marketplace_data: TabDataState<echo_agent_hooks_plugins_types::MarketplaceListResponse>,
    /// A marketplace list fetch is in flight. Overlapping list calls
    /// serialize on the shell's per-source cache lock and each re-scans
    /// every git source, so duplicates multiply the slowest source's latency.
    pub marketplace_fetch_inflight: bool,
    /// A refetch arrived while one was in flight; it runs when the current
    /// fetch lands so post-action results stay fresh.
    pub marketplace_refetch_queued: bool,
    pub marketplace_selected: usize,
    pub marketplace_scroll: usize,
    /// Skills tab state.
    pub skills_data: TabDataState<Vec<SkillInfo>>,
    pub skills_selected: usize,
    pub skills_scroll: usize,
    pub workflows_data: TabDataState<Vec<WorkflowInfo>>,
    /// MCP servers tab state.
    pub mcps_data: TabDataState<Vec<crate::views::mcps_modal::McpServerInfo>>,
    /// Last selection that triggered auto-scroll. Prevents mouse scroll
    /// from being overridden by auto-scroll on every render.
    pub mcps_scroll_pinned_selection: Option<usize>,
    pub mcps_scroll: usize,
    /// Expanded MCP server tool lists (by raw catalog `si`, not picker row index).
    pub mcps_tools_expanded: std::collections::HashSet<usize>,
    /// Collapsed MCP section headers (`mcp-section:*` keys). Key in set = collapsed.
    pub mcps_collapsed_sections: std::collections::HashSet<String>,
    /// Whether plugin section collapse defaults have been applied after first load.
    pub mcps_section_collapse_initialized: bool,
    /// Maps visible row offset to skill index (for mouse click).
    pub skills_visible_map: Vec<Option<usize>>,
    pub hooks_collapsed_groups: std::collections::HashSet<String>,
    /// Collapsed plugin source groups (by [`PluginGroup`] key).
    pub plugins_collapsed_groups: std::collections::HashSet<String>,
    /// See [`Self::seed_plugin_groups_once`].
    pub plugins_groups_seeded: bool,
    /// Collapsed marketplace sources (by source index). Default collapsed.
    pub marketplace_collapsed_sources: std::collections::HashSet<usize>,
    /// Collapsed skill source groups (by [`SkillGroup::label`]).
    pub skills_collapsed_groups: std::collections::HashSet<String>,
    /// See [`Self::seed_skills_groups_once`].
    pub skills_groups_seeded: bool,
    /// Expanded skill entries (by skill index). Skills start collapsed.
    pub skills_expanded: std::collections::HashSet<usize>,
    /// Status filter for the plugins tab.
    pub plugins_filter: StatusFilter,
    /// Status filter for the MCP servers tab.
    pub mcps_filter: StatusFilter,
    /// Status filter for the hooks tab.
    pub hooks_filter: StatusFilter,
    /// Status filter for the skills tab.
    pub skills_filter: StatusFilter,
    /// Unified picker state for tabs managed by `render_picker_content`.
    /// Search query and search_active live here (previously duplicated).
    pub picker_state: picker::PickerState,
    /// Maps picker entry index → original data index (for action dispatch).
    /// Rebuilt every render. `None` for headers or error entries.
    pub entry_data_indices: Vec<Option<usize>>,
    /// Cached entry labels from last render (for group header identification in input handler).
    pub entry_labels_cache: Vec<String>,
    /// Maps picker entry index → group key for collapse/expand.
    /// For hooks: the source_dir string. For plugins: the [`PluginGroup`]
    /// key. For marketplace: source index as string.
    /// `None` for non-group entries.
    pub entry_group_keys: Vec<Option<String>>,
    /// Per-entry keyboard/mouse selectability (rebuilt each render).
    pub entry_non_selectable: Vec<bool>,
    /// MCP section labels: not selectable, but clickable to fold/unfold.
    pub entry_non_selectable_clickable: Vec<bool>,
}

impl Default for ExtensionsModalState {
    fn default() -> Self {
        Self::new(ExtensionsTab::Hooks)
    }
}

impl ExtensionsModalState {
    /// Create a new modal state with the given initial tab.
    pub fn new(tab: ExtensionsTab) -> Self {
        Self {
            window: ModalWindowState::with_tabs(ExtensionsTab::ALL.len()),
            active_tab: tab,
            session_team_id: None,
            hooks_data: TabDataState::Loading,
            plugins_data: TabDataState::Loading,
            button_areas: Vec::new(),
            input: None,
            mcp_setup: None,
            modal_message: None,
            pending_action: None,
            pending_entry_index: None,
            result_notice: None,
            last_plugins_action: None,
            hooks_visible_map: Vec::new(),
            hooks_selected: 0,
            plugins_selected: 0,
            hooks_scroll: 0,
            plugins_scroll: 0,
            marketplace_data: TabDataState::Loading,
            marketplace_fetch_inflight: false,
            marketplace_refetch_queued: false,
            marketplace_selected: 0,
            marketplace_scroll: 0,
            skills_data: TabDataState::Loading,
            skills_selected: 0,
            skills_scroll: 0,
            workflows_data: TabDataState::Loading,
            mcps_data: TabDataState::Loading,
            mcps_scroll_pinned_selection: None,
            mcps_scroll: 0,
            mcps_tools_expanded: std::collections::HashSet::new(),
            // Plugin sections are seeded collapsed on first MCP load (Local
            // stays expanded by default for a less noisy initial view).
            // Section headers are keyboard-selectable so j/k lands on them
            // and Enter / l / Right re-expands a collapsed section.
            mcps_collapsed_sections: std::collections::HashSet::new(),
            mcps_section_collapse_initialized: false,
            skills_visible_map: Vec::new(),
            skills_expanded: std::collections::HashSet::new(),
            skills_collapsed_groups: std::collections::HashSet::new(),
            skills_groups_seeded: false,
            hooks_collapsed_groups: std::collections::HashSet::new(),
            plugins_collapsed_groups: std::collections::HashSet::new(),
            plugins_groups_seeded: false,
            marketplace_collapsed_sources: std::collections::HashSet::new(),
            plugins_filter: StatusFilter::default(),
            mcps_filter: StatusFilter::default(),
            hooks_filter: StatusFilter::default(),
            skills_filter: StatusFilter::default(),
            // PickerState mode is vestigial — ModalWindow handles framing.
            picker_state: picker::PickerState::default(),
            entry_data_indices: Vec::new(),
            entry_labels_cache: Vec::new(),
            entry_group_keys: Vec::new(),
            entry_non_selectable: Vec::new(),
            entry_non_selectable_clickable: Vec::new(),
        }
    }

    /// Advance the result-notice countdown by one animation tick. Returns
    /// `true` if it just expired (a redraw is needed to erase it).
    pub fn tick_result_notice(&mut self) -> bool {
        if let Some(ref mut n) = self.result_notice {
            if n.ticks_remaining == 0 {
                self.result_notice = None;
                return true;
            }
            n.ticks_remaining = n.ticks_remaining.saturating_sub(1);
        }
        false
    }

    /// Switch to a different tab and reset the per-tab transient UI state.
    ///
    /// Anything tied to the previous tab's data indices or modal flow
    /// (the Add form, an error/confirmation overlay, an in-flight
    /// `[processing]` badge, the picker selection / scroll / expansion
    /// state) is cleared so the new tab opens in a clean browse view.
    /// The user's search query (`picker_state.query()`) is intentionally
    /// preserved across tabs — current behavior elsewhere in the modal.
    pub fn switch_tab(&mut self, tab: ExtensionsTab) {
        self.active_tab = tab;
        // Clear modal flow state from the previous tab.
        self.input = None;
        self.mcp_setup = None;
        self.modal_message = None;
        self.pending_action = None;
        self.pending_entry_index = None;
        self.result_notice = None;
        // Reset picker selection/scroll/expansion for the new tab.
        // (Note: tabs_focused is *not* cleared here — it is orthogonal focus
        // state for the tab bar itself. L/R-driven tab switches want to keep
        // the bar focused so the user can continue cycling with arrows.)
        self.picker_state.selected = 0;
        self.picker_state.scroll_offset = None;
        self.picker_state.expanded.clear();
        self.mcps_tools_expanded.clear();
        self.picker_state.hovered = None;
    }

    /// Seed the all-collapsed default for plugin source groups exactly once.
    ///
    /// Called from both plugin-data delivery channels (list fetch and the
    /// `PluginsChanged` push); the first to deliver seeds, later deliveries
    /// preserve the user's expand state.
    pub fn seed_plugin_groups_once(
        &mut self,
        plugins: &[echo_agent_hooks_plugins_types::PluginInfo],
    ) {
        if self.plugins_groups_seeded {
            return;
        }
        self.plugins_collapsed_groups = plugins.iter().map(|p| plugin_group(p).key).collect();
        self.plugins_groups_seeded = true;
    }

    /// Seed default-collapsed skill source groups exactly once.
    ///
    /// Unions [`SkillGroup::label`] keys into `skills_collapsed_groups`;
    /// later reloads leave the set alone so user expand state survives.
    pub fn seed_skills_groups_once(&mut self, skills: &[SkillInfo]) {
        if self.skills_groups_seeded {
            return;
        }
        for skill in skills {
            self.skills_collapsed_groups
                .insert(skill_group(skill).label);
        }
        self.skills_groups_seeded = true;
    }

    /// Whether a group header at picker index `sel` with the given
    /// `group_key` is currently expanded (children visible).
    pub fn is_group_expanded(&self, sel: usize, group_key: &str) -> bool {
        let searching = !self.picker_state.query().is_empty();

        match self.active_tab {
            // During active search we force all hook groups open so matches
            // inside previously-collapsed groups are visible.
            ExtensionsTab::Hooks => searching || !self.hooks_collapsed_groups.contains(group_key),
            ExtensionsTab::Plugins => {
                searching || !self.plugins_collapsed_groups.contains(group_key)
            }
            ExtensionsTab::Skills => searching || !self.skills_collapsed_groups.contains(group_key),
            // Workflows tab is a flat list without collapsible groups.
            ExtensionsTab::Workflows => false,
            ExtensionsTab::Marketplace => {
                let source_has_error = group_key
                    .parse::<usize>()
                    .ok()
                    .and_then(|si| {
                        if let TabDataState::Loaded(ref data) = self.marketplace_data {
                            data.sources.get(si).and_then(|s| s.error.as_ref())
                        } else {
                            None
                        }
                    })
                    .is_some();
                if source_has_error {
                    // Error sources use the shared picker_state.expanded
                    // (already handled by the general search-expand logic).
                    self.picker_state.expanded.contains(&sel)
                } else {
                    // Normal marketplace sources: force open while searching.
                    searching
                        || group_key
                            .parse::<usize>()
                            .ok()
                            .is_none_or(|si| !self.marketplace_collapsed_sources.contains(&si))
                }
            }
            ExtensionsTab::McpServers => {
                if group_key.starts_with("mcp-section:") {
                    searching || !self.mcps_collapsed_sections.contains(group_key)
                } else if let Some(si) = parse_mcp_tools_server_index(group_key) {
                    self.mcps_tools_expanded.contains(&si)
                } else {
                    false
                }
            }
        }
    }

    /// Apply pasted text to the focused input field or the search query.
    ///
    /// Strips `\n` and `\r`. Returns `true` if any state was modified.
    pub fn apply_paste(&mut self, text: &str) -> bool {
        if let Some(ref mut input) = self.input {
            input.insert_paste(text)
        } else if self.picker_state.search_active {
            matches!(
                self.picker_state.paste_query(text),
                crate::input::line_editor::LineEditOutcome::TextChanged
            )
        } else {
            false
        }
    }

    /// Resolve the current picker selection to the original data index.
    /// Returns `None` if the selection is on a header or out of range.
    pub fn selected_data_index(&self) -> Option<usize> {
        data_index_at(&self.entry_data_indices, self.picker_state.selected)
    }

    pub fn selected_item_enabled(&self) -> Option<bool> {
        selected_item_enabled_at(
            self,
            &self.entry_data_indices,
            &self.entry_group_keys,
            self.picker_state.selected,
        )
    }

    /// Resolve the picker selection to `(server_index, tool_index)` when the
    /// cursor is on an MCP tool row. Returns `None` on server rows, error/
    /// loading entries, or out-of-range. Caller must be on the McpServers tab
    /// — the helper relies on no other tab using `"mcp-tools:"` as a group-key prefix.
    pub fn selected_mcp_tool(&self) -> Option<(usize, usize)> {
        selected_mcp_tool_at(
            &self.entry_data_indices,
            &self.entry_group_keys,
            self.picker_state.selected,
        )
    }

    /// For the Marketplace tab, resolve the currently selected picker entry to
    /// the source and (optionally) the plugin within that source.
    /// Returns `(source_index, Option<plugin_index_within_source>)`.
    pub fn resolve_marketplace_selection(
        &self,
        sources: &[echo_agent_hooks_plugins_types::MarketplaceScanResult],
    ) -> Option<(usize, Option<usize>)> {
        let sel = self.picker_state.selected;
        // Source index from entry_data_indices (None for source headers).
        let source_idx =
            if let Some(group_key) = self.entry_group_keys.get(sel).and_then(|k| k.as_ref()) {
                // Source header — group_key is the source index.
                group_key.parse::<usize>().ok()?
            } else {
                // Plugin entry — data index is the source index.
                self.entry_data_indices.get(sel)?.as_ref().copied()?
            };
        let source = sources.get(source_idx)?;
        // Check if this is a source header (has group key) or a plugin entry.
        if self
            .entry_group_keys
            .get(sel)
            .and_then(|k| k.as_ref())
            .is_some()
        {
            return Some((source_idx, None));
        }
        // Match plugin by name (entry_labels_cache stores plugin.name for plugin entries).
        let label = self.entry_labels_cache.get(sel)?;
        let plugin_idx = source.plugins.iter().position(|p| p.name == *label)?;
        Some((source_idx, Some(plugin_idx)))
    }

    /// True when the user is expanding an auth-required server's tool list (OAuth
    /// should run instead of fold). False when collapsing or when tools are already
    /// expanded.
    pub fn mcp_auth_intercept_on_expand(&self) -> bool {
        if self.active_tab != ExtensionsTab::McpServers {
            return false;
        }
        let Some(group_key) = self
            .entry_group_keys
            .get(self.picker_state.selected)
            .and_then(|k| k.as_ref())
        else {
            return false;
        };
        let Some(si) = parse_mcp_tools_server_index(group_key) else {
            return false;
        };
        if self.mcps_tools_expanded.contains(&si) {
            return false;
        }
        matches!(
            &self.mcps_data,
            TabDataState::Loaded(servers) if {
                servers.get(si).is_some_and(|srv| srv.auth_required)
            }
        )
    }
}

/// Parse raw server index from an `mcp-tools:{si}` group key.
pub(crate) fn parse_mcp_tools_server_index(group_key: &str) -> Option<usize> {
    group_key.strip_prefix("mcp-tools:")?.parse().ok()
}

/// Build the picker non-selectable mask (static headers).
///
/// MCP section labels (`mcp-section:*`) are keyboard-selectable so j/k can
/// land on them and Enter / l / Right toggles their collapsed state — the
/// only way to expand a section once it has been collapsed.
pub fn build_entry_non_selectable(
    entry_is_header: &[bool],
    _entry_group_keys: &[Option<String>],
) -> Vec<bool> {
    entry_is_header.to_vec()
}

/// MCP section labels are now keyboard-selectable, so no rows need the
/// "non-selectable but clickable" treatment. Kept as a function so callers
/// can continue to pass a slice to the picker without per-call allocation
/// changes; the returned mask is all `false`.
pub fn build_entry_non_selectable_clickable(entry_group_keys: &[Option<String>]) -> Vec<bool> {
    vec![false; entry_group_keys.len()]
}

/// Picker rows built for the MCP servers tab (labels + mapping only).
///
/// Used by the full extensions-modal tests and by minimal mode's below-prompt
/// MCP list (`crate::minimal::panel`), which reuses this exact ordering so the
/// shared `picker_state.selected` (driven by the unchanged input handler) lines
/// up with the rendered rows.
#[derive(Debug, Default)]
pub(crate) struct McpServersPickerRows {
    pub(crate) labels: Vec<String>,
    pub(crate) group_keys: Vec<Option<String>>,
    pub(crate) data_indices: Vec<Option<usize>>,
}

/// Build MCP picker rows (section headers, servers, optional tool children).
pub(crate) fn build_mcp_servers_picker_rows(
    servers: &[crate::views::mcps_modal::McpServerInfo],
    query: &str,
    filter: StatusFilter,
    collapsed_sections: &std::collections::HashSet<String>,
    tools_expanded: &std::collections::HashSet<usize>,
) -> McpServersPickerRows {
    use crate::views::mcps_modal::{McpSectionId, section_for, section_key, section_label};

    let searching = !query.is_empty();
    let mut sections: std::collections::BTreeMap<
        McpSectionId,
        Vec<(usize, &crate::views::mcps_modal::McpServerInfo)>,
    > = std::collections::BTreeMap::new();
    for (si, server) in servers.iter().enumerate() {
        let display_name = server.display_name.as_deref().unwrap_or(&server.name);
        if !fuzzy_matches(display_name, query) {
            continue;
        }
        if !filter.matches(server.enabled) {
            continue;
        }
        sections
            .entry(section_for(server))
            .or_default()
            .push((si, server));
    }

    let mut out = McpServersPickerRows::default();
    for (section_id, section_servers) in &sections {
        let sec_key = section_key(section_id);
        let section_collapsed =
            mcp_section_children_hidden(collapsed_sections, &sec_key, searching);
        out.labels
            .push(section_label(section_id, section_servers.len()));
        out.data_indices.push(None);
        out.group_keys.push(Some(sec_key));
        if section_collapsed {
            continue;
        }
        for &(si, server) in section_servers {
            out.labels.push(
                server
                    .display_name
                    .as_deref()
                    .unwrap_or(&server.name)
                    .to_string(),
            );
            out.data_indices.push(Some(si));
            out.group_keys.push(Some(format!("mcp-tools:{si}")));
            if tools_expanded.contains(&si) {
                for t in &server.tools {
                    out.labels
                        .push(t.display_name.clone().unwrap_or_else(|| t.name.clone()));
                    out.data_indices.push(Some(si));
                    out.group_keys.push(None);
                }
            }
        }
    }
    out
}

/// On first MCP list load, collapse each distinct plugin section by default.
pub(crate) fn init_mcps_section_collapse_on_first_load(
    collapsed_sections: &mut std::collections::HashSet<String>,
    initialized: &mut bool,
    servers: &[crate::views::mcps_modal::McpServerInfo],
) {
    if *initialized {
        return;
    }
    use crate::views::mcps_modal::{McpSectionId, section_for, section_key};
    for server in servers {
        if let McpSectionId::Plugin(ref name) = section_for(server) {
            collapsed_sections.insert(section_key(&McpSectionId::Plugin(name.clone())));
        }
    }
    *initialized = true;
}

/// Seed the MCP section collapse map for a post-CTA-install handoff: collapse
/// Managed, Local, and every plugin section EXCEPT `target_plugin`, then mark
/// the map initialized so the default first-load seeder no-ops. Leaves only the
/// just-installed plugin's section expanded for the auth step.
pub(crate) fn seed_mcps_section_collapse_for_cta(
    collapsed_sections: &mut std::collections::HashSet<String>,
    initialized: &mut bool,
    servers: &[crate::views::mcps_modal::McpServerInfo],
    target_plugin: &str,
) {
    use crate::views::mcps_modal::{McpSectionId, section_for, section_key};
    let target = section_key(&McpSectionId::Plugin(target_plugin.to_string()));
    collapsed_sections.insert(section_key(&McpSectionId::Managed));
    collapsed_sections.insert(section_key(&McpSectionId::Local));
    for server in servers {
        let key = section_key(&section_for(server));
        if key != target {
            collapsed_sections.insert(key);
        }
    }
    collapsed_sections.remove(&target);
    *initialized = true;
}

/// Whether an MCP section's child servers are hidden (mirrors render logic).
pub(crate) fn mcp_section_children_hidden(
    collapsed_sections: &std::collections::HashSet<String>,
    section_key: &str,
    searching: bool,
) -> bool {
    !searching && collapsed_sections.contains(section_key)
}

/// Derive a display label and whether the source is a custom (removable) path.
///
/// Returns `(label, is_custom)` where `is_custom` means the source was added
/// via hooks-paths and can be removed.
pub fn derive_source_label(source_dir: &str) -> (String, bool) {
    let meta = classify_hook_source(source_dir);
    let is_custom = meta.is_custom();
    (meta.label, is_custom)
}

/// Classify a hooks `source_dir` into a display label and stable kind rank.
fn classify_hook_source(source_dir: &str) -> HookSourceMeta {
    let echoagent = echo_agent_config::echo_agent_home();
    let source_path = std::path::Path::new(source_dir);
    // Plugin / installed-plugin dirs, under the user echoagent home (ECHO_AGENT_HOME-aware)
    // or a project-scoped `{cwd}/.echo-agent/<subdir>/`. Returns the first path
    // component after the subdir (the plugin's install directory name).
    let plugin_name = |subdir: &str| -> Option<String> {
        let first_comp = |p: &std::path::Path| {
            p.components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
        };
        // User echoagent home (ECHO_AGENT_HOME-aware).
        if let Ok(rest) = source_path.strip_prefix(echoagent.join(subdir))
            && let Some(name) = first_comp(rest)
        {
            return Some(name);
        }
        // Project-scoped `.echo-agent/<subdir>/<name>` anywhere in the path.
        // Component-based so it works regardless of path separator.
        let comps: Vec<_> = source_path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        comps
            .windows(3)
            .find(|w| w[0] == ".echo-agent" && w[1] == subdir && !w[2].is_empty())
            .map(|w| w[2].clone())
    };
    if let Some(name) = plugin_name("plugins").or_else(|| plugin_name("installed-plugins")) {
        return HookSourceMeta {
            label: format!("Plugin: {name}"),
            kind: HookSourceKind::Plugin,
        };
    }
    // Global hooks under $ECHO_AGENT_HOME/hooks
    let global_hooks = echoagent.join("hooks");
    let global_str = global_hooks.display().to_string();
    if source_dir == global_str || source_dir.starts_with(&format!("{global_str}/")) {
        return HookSourceMeta {
            label: "Global hooks".into(),
            kind: HookSourceKind::Global,
        };
    }
    // Settings under .claude/
    if source_dir.contains("/.claude/") {
        return HookSourceMeta {
            label: "Claude settings".into(),
            kind: HookSourceKind::Claude,
        };
    }
    // Project hooks
    if source_dir.ends_with("/.echo-agent/hooks") || source_dir.contains("/.echo-agent/hooks/") {
        return HookSourceMeta {
            label: "Project hooks".into(),
            kind: HookSourceKind::Project,
        };
    }
    // Custom directory — removable
    let display = {
        if let Ok(rest) = source_path.strip_prefix(&echoagent) {
            let prefix = crate::util::display_echo_agent_home_prefix();
            let rest_str = rest.to_string_lossy();
            let rest_trimmed = rest_str.strip_prefix('/').unwrap_or(&rest_str);
            format!("Custom: {prefix}/{rest_trimmed}")
        } else if let Some(home) = dirs::home_dir() {
            let home_str = home.display().to_string();
            source_dir
                .strip_prefix(&home_str)
                .map(|rest| format!("Custom: ~{rest}"))
                .unwrap_or_else(|| format!("Custom: {source_dir}"))
        } else {
            format!("Custom: {source_dir}")
        }
    };
    HookSourceMeta {
        label: display,
        kind: HookSourceKind::Custom,
    }
}

// ---------------------------------------------------------------------------
// Entry builders — convert tab data into Vec<PickerEntry> for render_picker
// ---------------------------------------------------------------------------

/// One skill row after filter + group-then-A–Z sort.
#[derive(Debug, Clone)]
struct SkillMatch {
    skill_index: usize,
    /// True when the query hit name/label/author (not description-only).
    name_hit: bool,
    group_rank: u8,
    group_label: String,
}

/// Ordered skill matches for the Skills tab (group labels included).
struct SkillsEntryData {
    matches: Vec<SkillMatch>,
}

fn filter_and_sort_skills(
    skills: &[SkillInfo],
    query: &str,
    filter: StatusFilter,
) -> SkillsEntryData {
    let mut matches: Vec<SkillMatch> = Vec::new();
    let query_lower = query.to_lowercase();
    for (si, skill) in skills.iter().enumerate() {
        if !filter.matches(skill.enabled) {
            continue;
        }
        let group = skill_group(skill);
        if query.is_empty() {
            matches.push(SkillMatch {
                skill_index: si,
                name_hit: true,
                group_rank: group.rank,
                group_label: group.label,
            });
            continue;
        }
        let desc_text = skill
            .short_description
            .as_deref()
            .unwrap_or(&skill.description);
        let desc_lower = desc_text.to_lowercase();
        let author_lower = skill.author.as_deref().unwrap_or("").to_lowercase();
        // Plugin skills differ in label (shown) vs name (slash id); match either.
        let name_hit = skill.label().to_lowercase().contains(&query_lower)
            || skill.name.to_lowercase().contains(&query_lower);
        let desc_hit = desc_lower.contains(&query_lower);
        let author_hit = !author_lower.is_empty() && author_lower.contains(&query_lower);
        if name_hit || author_hit {
            matches.push(SkillMatch {
                skill_index: si,
                name_hit: true,
                group_rank: group.rank,
                group_label: group.label,
            });
        } else if desc_hit {
            matches.push(SkillMatch {
                skill_index: si,
                name_hit: false,
                group_rank: group.rank,
                group_label: group.label,
            });
        }
    }
    // Rank → case-insensitive group → exact label (stable when case differs) →
    // name hits before desc-only → A–Z label. Renderer keys headers on exact
    // `group_label`, so case-differing names stay separate groups.
    matches.sort_by_cached_key(|m| {
        (
            m.group_rank,
            m.group_label.to_lowercase(),
            m.group_label.clone(),
            !m.name_hit,
            skills[m.skill_index].label().to_lowercase(),
            m.skill_index,
        )
    });
    SkillsEntryData { matches }
}

fn skill_source_str(skill: &SkillInfo) -> String {
    if let Some(ref cs) = skill.config_source {
        match cs {
            echo_agent_tools::types::config_source::ConfigSource::User { path } => {
                if crate::util::is_under_user_echo_agent_home(path) {
                    crate::util::display_user_echo_agent_path("skills")
                } else if path.display().to_string().contains("/.claude/") {
                    "~/.claude/skills".into()
                } else {
                    "user".into()
                }
            }
            echo_agent_tools::types::config_source::ConfigSource::Project { path } => {
                let s = path.display().to_string();
                if s.contains("/.echo-agent/") {
                    ".echo-agent/skills".into()
                } else if s.contains("/.claude/") {
                    ".claude/skills".into()
                } else {
                    "project".into()
                }
            }
            echo_agent_tools::types::config_source::ConfigSource::Plugin {
                plugin_name, ..
            } => {
                format!("plugin: {}", plugin_name)
            }
            _ => format!("{:?}", skill.scope).to_lowercase(),
        }
    } else {
        format!("{:?}", skill.scope).to_lowercase()
    }
}

/// Build picker fields for an expanded plugin.
fn build_plugin_fields(plugin: &echo_agent_hooks_plugins_types::PluginInfo) -> Vec<String> {
    use echo_agent_hooks_plugins_types::McpStatus;
    let mut components = Vec::new();
    if !plugin.skill_names.is_empty() {
        components.push(format!("skills: {}", plugin.skill_names.join(", ")));
    } else if plugin.skill_count > 0 {
        components.push(format!("{} skills", plugin.skill_count));
    }
    if !plugin.agent_names.is_empty() {
        components.push(format!("agents: {}", plugin.agent_names.join(", ")));
    } else if plugin.agent_count > 0 {
        components.push(format!("{} agents", plugin.agent_count));
    }
    if plugin.hook_count > 0 {
        components.push(format!("{} hooks", plugin.hook_count));
    }
    match plugin.mcp_status {
        McpStatus::Active | McpStatus::ActiveInline => {
            components.push(format!("{} MCP servers", plugin.mcp_server_count));
        }
        McpStatus::Blocked => {
            components.push(format!("{} MCP: blocked", plugin.mcp_server_count));
        }
        McpStatus::None => {}
    }
    components
}

/// Names shown per component category before "+N more".
const COMPONENT_ITEMS_CAP: usize = 8;

/// Copy for a catalog entry verified to provide nothing detectable.
const NO_DETECTABLE_COMPONENTS: &str = "no detectable components";

fn component_categories(
    components: &echo_agent_hooks_plugins_types::PluginComponents,
) -> [(
    &'static str,
    &[echo_agent_hooks_plugins_types::ComponentItem],
); 6] {
    use echo_agent_hooks_plugins_types::ComponentCategory;
    components.categories().map(|(category, items)| {
        let label = match category {
            ComponentCategory::Skills => "skills",
            ComponentCategory::Commands => "commands",
            ComponentCategory::Agents => "agents",
            ComponentCategory::McpServers => "mcp servers",
            ComponentCategory::Hooks => "hooks",
            ComponentCategory::LspServers => "lsp servers",
        };
        (label, items)
    })
}

/// Per-category names-only fields for an expanded marketplace entry:
/// comma-joined component names, capped per category with "+N more".
pub(crate) fn render_components_fields(
    components: &echo_agent_hooks_plugins_types::PluginComponents,
) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for (label, items) in component_categories(components) {
        if items.is_empty() {
            continue;
        }
        let names: Vec<&str> = items
            .iter()
            .take(COMPONENT_ITEMS_CAP)
            .map(|item| item.name.as_str())
            .collect();
        let mut value = names.join(", ");
        if items.len() > COMPONENT_ITEMS_CAP {
            value.push_str(&format!(" +{} more", items.len() - COMPONENT_ITEMS_CAP));
        }
        fields.push((label.to_string(), value));
    }
    fields
}

/// Collapsed-row summary from catalog components; `None` without catalog data.
pub(crate) fn marketplace_components_summary(
    plugin: &echo_agent_hooks_plugins_types::MarketplacePluginEntry,
) -> Option<String> {
    plugin
        .components
        .as_ref()
        .and_then(|components| components.summary_line())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the hooks/plugins modal popup as a centered overlay.
///
/// Uses the shared [`ModalWindow`](super::modal_window) for chrome
/// (border, title, close button, tab bar, footer shortcuts) and
/// [`render_picker_content`](picker::render_picker_content) for the
/// scrollable entry list inside.
///
/// `full_area` is the total area available (everything above the shortcuts bar).
/// Show each spinner frame for this many animation ticks.
const SPINNER_DIVISOR: u64 = 4;

pub fn render_extensions_modal(
    buf: &mut Buffer,
    full_area: Rect,
    state: &mut ExtensionsModalState,
    _shortcuts_area: Option<Rect>,
    compact: bool,
    tick: u64,
) {
    let theme = Theme::current();

    // Guard: if terminal is too small, bail.
    if full_area.width < 40 || full_area.height < 12 {
        state.button_areas.clear();
        return;
    }

    // When switching into (or rendering) a tab while a search query is active,
    // force-expand all items of that tab. This ensures search filtering shows
    // every match explicitly, even inside groups that were collapsed before
    // the user switched tabs.
    if !state.picker_state.query().is_empty() {
        state.picker_state.expand_all_for_search(8192);
    }

    // Tab labels and active index.
    let labels: Vec<&str> = ExtensionsTab::ALL.iter().map(|t| t.label()).collect();
    let active_idx = ExtensionsTab::ALL
        .iter()
        .position(|t| *t == state.active_tab)
        .unwrap_or(0);
    state.window.active_tab = active_idx;
    state.window.tabs_focused = state.picker_state.tabs_focused;

    // Determine filter for current tab.
    let has_filter = matches!(
        state.active_tab,
        ExtensionsTab::Hooks
            | ExtensionsTab::Plugins
            | ExtensionsTab::McpServers
            | ExtensionsTab::Skills
    );
    let filter = match state.active_tab {
        ExtensionsTab::Hooks => state.hooks_filter,
        ExtensionsTab::Plugins => state.plugins_filter,
        ExtensionsTab::McpServers => state.mcps_filter,
        ExtensionsTab::Skills => state.skills_filter,
        _ => StatusFilter::All,
    };

    // Determine if this tab is loading.
    let loading = match state.active_tab {
        ExtensionsTab::Hooks => matches!(state.hooks_data, TabDataState::Loading),
        ExtensionsTab::Plugins => matches!(state.plugins_data, TabDataState::Loading),
        ExtensionsTab::Marketplace => matches!(state.marketplace_data, TabDataState::Loading),
        ExtensionsTab::Skills => matches!(state.skills_data, TabDataState::Loading),
        ExtensionsTab::Workflows => matches!(state.workflows_data, TabDataState::Loading),
        ExtensionsTab::McpServers => matches!(state.mcps_data, TabDataState::Loading),
    };

    // Input mode hides the entry list (form overlay owns the content area).
    let in_input_mode = state.input.is_some() || state.mcp_setup.is_some();

    // Rebuild the entry list *before* footer action labels so Space
    // enable/disable can use this frame's mapping (passed as locals to
    // `action_key_footer_desc_for_mapping`), not last frame's filter/tab/query.
    // ── Build PickerEntry list for current tab ──
    // We build owned data here and reference it for the picker.
    let mut entry_labels: Vec<String> = Vec::new();
    let mut entry_right_labels: Vec<String> = Vec::new();
    let mut entry_desc_lines: Vec<Vec<String>> = Vec::new();
    let mut entry_summary_lines: Vec<Vec<String>> = Vec::new();
    let mut entry_fields: Vec<Vec<(String, String)>> = Vec::new();
    let mut entry_is_header: Vec<bool> = Vec::new();
    let mut entry_dimmed: Vec<bool> = Vec::new();
    let mut entry_indent: Vec<u8> = Vec::new();
    let mut entry_group_keys: Vec<Option<String>> = Vec::new();
    let mut entry_badge_text: Vec<String> = Vec::new();
    let mut entry_badge_color: Vec<Option<ratatui::style::Color>> = Vec::new();
    // Maps picker entry index → original data index (for action dispatch).
    let mut entry_data_indices: Vec<Option<usize>> = Vec::new();

    // Skip building entries when in input mode (render_input_form handles that).
    if !in_input_mode && !loading {
        match state.active_tab {
            ExtensionsTab::Skills => {
                if let TabDataState::Loaded(ref skills) = state.skills_data {
                    let filtered =
                        filter_and_sort_skills(skills, state.picker_state.query(), filter);
                    let searching = !state.picker_state.query().is_empty();
                    // Bucket matches by group while preserving filter order.
                    let mut group_order: Vec<String> = Vec::new();
                    let mut by_group: std::collections::HashMap<String, Vec<&SkillMatch>> =
                        std::collections::HashMap::new();
                    for m in &filtered.matches {
                        if !by_group.contains_key(&m.group_label) {
                            group_order.push(m.group_label.clone());
                        }
                        by_group.entry(m.group_label.clone()).or_default().push(m);
                    }
                    for group_label in &group_order {
                        let members = by_group
                            .get(group_label)
                            .map(|v| v.as_slice())
                            .unwrap_or(&[]);
                        let collapsed =
                            !searching && state.skills_collapsed_groups.contains(group_label);
                        let count = members.len();
                        entry_labels.push(if count == 1 {
                            format!("{group_label} (1 skill)")
                        } else {
                            format!("{group_label} ({count} skills)")
                        });
                        entry_right_labels.push(String::new());
                        entry_desc_lines.push(vec![]);
                        entry_summary_lines.push(vec![]);
                        entry_fields.push(vec![]);
                        // Selectable collapsible header (same as MCP / plugins).
                        entry_is_header.push(false);
                        entry_dimmed.push(false);
                        entry_indent.push(0);
                        entry_data_indices.push(None);
                        entry_group_keys.push(Some(group_label.clone()));
                        entry_badge_text.push(String::new());
                        entry_badge_color.push(None);
                        if collapsed {
                            continue;
                        }
                        for m in members {
                            let si = m.skill_index;
                            let skill = &skills[si];
                            let source = skill_source_str(skill);
                            entry_labels.push(skill.label().to_string());
                            let right = match &skill.author {
                                Some(a) if !a.is_empty() => format!("({} · {})", source, a),
                                _ => format!("({})", source),
                            };
                            entry_right_labels.push(right);
                            let desc = skill
                                .short_description
                                .as_deref()
                                .unwrap_or(&skill.description);
                            if desc.is_empty() {
                                entry_desc_lines.push(vec![]);
                            } else {
                                entry_desc_lines.push(vec![desc.to_string()]);
                            }
                            entry_summary_lines.push(vec![]);
                            let mut fields = vec![("path".to_string(), skill.path.clone())];
                            if let Some(ref a) = skill.author
                                && !a.is_empty()
                            {
                                fields.push(("author".to_string(), a.clone()));
                            }
                            if let Some(ref tools) = skill.allowed_tools
                                && !tools.is_empty()
                            {
                                fields.push(("tools".to_string(), tools.join(", ")));
                            }
                            entry_fields.push(fields);
                            entry_is_header.push(false);
                            entry_dimmed.push(!skill.enabled);
                            entry_indent.push(1);
                            entry_data_indices.push(Some(si));
                            entry_group_keys.push(None);
                            if !skill.enabled {
                                entry_badge_text.push("[disabled]".into());
                                entry_badge_color.push(Some(theme.accent_error));
                            } else {
                                entry_badge_text.push(String::new());
                                entry_badge_color.push(None);
                            }
                        }
                    }
                } else if let TabDataState::Error(ref msg) = state.skills_data {
                    entry_labels.push(format!("Error: {}", msg));
                    entry_right_labels.push(String::new());
                    entry_desc_lines.push(vec![]);
                    entry_summary_lines.push(vec![]);
                    entry_fields.push(vec![]);
                    entry_is_header.push(false);
                    entry_dimmed.push(false);
                    entry_indent.push(0);
                    entry_data_indices.push(None);
                    entry_group_keys.push(None);
                    entry_badge_text.push(String::new());
                    entry_badge_color.push(None);
                }
            }
            ExtensionsTab::Workflows => {
                // The builder owns all data states (loaded/empty/error).
                for row in
                    build_workflows_picker_rows(&state.workflows_data, state.picker_state.query())
                {
                    entry_labels.push(row.label);
                    entry_right_labels.push(row.right_label);
                    entry_desc_lines.push(row.desc_lines);
                    entry_summary_lines.push(vec![]);
                    entry_fields.push(row.fields);
                    entry_is_header.push(false);
                    entry_dimmed.push(row.dimmed);
                    entry_indent.push(0);
                    // Browse-only rows: no data indices, no group keys.
                    entry_data_indices.push(None);
                    entry_group_keys.push(None);
                    entry_badge_text.push(String::new());
                    entry_badge_color.push(None);
                }
            }
            ExtensionsTab::Plugins => {
                if let TabDataState::Loaded(ref response) = state.plugins_data {
                    let mut groups = GroupedPlugins::new();
                    for (pi, plugin) in response.plugins.iter().enumerate() {
                        if !fuzzy_matches(&plugin.name, state.picker_state.query()) {
                            continue;
                        }
                        if !filter.matches(plugin.enabled) {
                            continue;
                        }
                        let group = plugin_group(plugin);
                        groups
                            .entry(group.into_sort_key())
                            .or_default()
                            .push((pi, plugin));
                    }
                    for plugins in groups.values_mut() {
                        plugins.sort_by(|a, b| {
                            cmp_str_ci(&a.1.name, &b.1.name).then_with(|| a.0.cmp(&b.0))
                        });
                    }
                    for (group_key, plugins) in &groups {
                        let label = &group_key.label;
                        let group_key = &group_key.key;
                        // While searching we ignore previous collapse state so
                        // every plugin inside the group can be seen and matched.
                        let searching = !state.picker_state.query().is_empty();
                        let collapsed =
                            !searching && state.plugins_collapsed_groups.contains(group_key);
                        entry_labels.push(format!(
                            "{} ({})",
                            label,
                            plugin_count_label(plugins.len())
                        ));
                        entry_right_labels.push(String::new());
                        entry_desc_lines.push(vec![]);
                        entry_summary_lines.push(vec![]);
                        entry_fields.push(vec![]);
                        entry_is_header.push(false); // group header, but selectable
                        entry_dimmed.push(false);
                        entry_indent.push(0);
                        entry_data_indices.push(None);
                        entry_group_keys.push(Some(group_key.clone()));
                        entry_badge_text.push(String::new());
                        entry_badge_color.push(None);
                        if collapsed {
                            continue;
                        }
                        for &(pi, plugin) in plugins {
                            let version_str = plugin
                                .version
                                .as_ref()
                                .map(|v| format!(" v{v}"))
                                .unwrap_or_default();
                            entry_labels.push(format!("{}{}", plugin.name, version_str));
                            entry_right_labels.push(String::new());
                            // Build description lines from components.
                            let components = build_plugin_fields(plugin);
                            if components.is_empty() {
                                entry_desc_lines.push(vec![]);
                            } else {
                                entry_desc_lines.push(vec![components.join("  ")]);
                            }
                            entry_summary_lines.push(vec![]);
                            // Fields for expanded view.
                            let mut fields = Vec::new();
                            if let Some(ref desc) = plugin.description
                                && !desc.is_empty()
                            {
                                fields.push(("description".to_string(), desc.clone()));
                            }
                            fields.push(("path".to_string(), plugin.root.clone()));
                            entry_fields.push(fields);
                            entry_is_header.push(false);
                            entry_dimmed.push(!plugin.enabled);
                            entry_indent.push(1);
                            entry_data_indices.push(Some(pi));
                            entry_group_keys.push(None);
                            entry_badge_text.push(if !plugin.enabled {
                                "[disabled]".into()
                            } else {
                                String::new()
                            });
                            entry_badge_color.push(if !plugin.enabled {
                                Some(theme.accent_error)
                            } else {
                                None
                            });
                        }
                    }
                } else if let TabDataState::Error(ref msg) = state.plugins_data {
                    entry_labels.push(format!("Error: {}", msg));
                    entry_right_labels.push(String::new());
                    entry_desc_lines.push(vec![]);
                    entry_summary_lines.push(vec![]);
                    entry_fields.push(vec![]);
                    entry_is_header.push(false);
                    entry_dimmed.push(false);
                    entry_indent.push(0);
                    entry_data_indices.push(None);
                    entry_group_keys.push(None);
                    entry_badge_text.push(String::new());
                    entry_badge_color.push(None);
                }
            }
            ExtensionsTab::Hooks => {
                if let TabDataState::Loaded(ref data) = state.hooks_data {
                    let groups = build_hook_groups(
                        &data.hooks,
                        state.hooks_filter,
                        state.picker_state.query(),
                    );
                    for group in &groups {
                        let source_dir = group.source_dir;
                        let label = &group.label;
                        let indices = &group.indices;
                        // While searching we ignore previous collapse state so
                        // every hook inside the group can be seen and matched.
                        let searching = !state.picker_state.query().is_empty();
                        let collapsed =
                            !searching && state.hooks_collapsed_groups.contains(source_dir);
                        entry_labels.push(format!("{} ({} hooks)", label, indices.len()));
                        entry_right_labels.push(String::new());
                        entry_desc_lines.push(vec![]);
                        entry_summary_lines.push(vec![]);
                        entry_fields.push(vec![]);
                        entry_is_header.push(false); // group header, but selectable
                        entry_dimmed.push(false); // headers
                        entry_indent.push(0);
                        entry_data_indices.push(None);
                        entry_group_keys.push(Some(source_dir.to_string()));
                        entry_badge_text.push(String::new());
                        entry_badge_color.push(None);
                        if collapsed {
                            continue;
                        }
                        for &hi in indices {
                            let hook = &data.hooks[hi];
                            entry_labels.push(hook_row_label(hook));
                            let cmd = hook
                                .command
                                .as_deref()
                                .unwrap_or(hook.url.as_deref().unwrap_or("(no command)"));
                            entry_right_labels.push(String::new());
                            entry_desc_lines.push(vec![format!("\u{2192} {}", cmd)]);
                            entry_summary_lines.push(vec![]);
                            entry_fields.push(vec![]);
                            entry_is_header.push(false);
                            entry_dimmed.push(hook.disabled);
                            entry_indent.push(1);
                            entry_data_indices.push(Some(hi));
                            entry_group_keys.push(None);
                            entry_badge_text.push(if hook.disabled {
                                "[disabled]".into()
                            } else {
                                String::new()
                            });
                            entry_badge_color.push(if hook.disabled {
                                Some(theme.accent_error)
                            } else {
                                None
                            });
                        }
                    }
                } else if let TabDataState::Error(ref msg) = state.hooks_data {
                    entry_labels.push(format!("Error: {}", msg));
                    entry_right_labels.push(String::new());
                    entry_desc_lines.push(vec![]);
                    entry_summary_lines.push(vec![]);
                    entry_fields.push(vec![]);
                    entry_is_header.push(false);
                    entry_dimmed.push(false);
                    entry_indent.push(0);
                    entry_data_indices.push(None);
                    entry_group_keys.push(None);
                    entry_badge_text.push(String::new());
                    entry_badge_color.push(None);
                }
            }
            ExtensionsTab::Marketplace => {
                if let TabDataState::Loaded(ref data) = state.marketplace_data {
                    for view in ordered_marketplace_view(&data.sources) {
                        let si = view.source_index;
                        let plugin_order = &view.plugin_indices;
                        let source = &data.sources[si];
                        // Force all marketplace sources open while searching so their
                        // plugins are considered for matching and displayed.
                        let searching = !state.picker_state.query().is_empty();
                        let collapsed =
                            !searching && state.marketplace_collapsed_sources.contains(&si);
                        entry_labels.push(format!(
                            "{} ({})",
                            source.source_name,
                            plugin_count_label(source.plugins.len())
                        ));
                        entry_right_labels.push(String::new());
                        entry_desc_lines.push(vec![]);
                        entry_summary_lines.push(vec![]);
                        if let Some(ref err) = source.error {
                            entry_fields.push(vec![("error".to_string(), err.clone())]);
                        } else {
                            entry_fields.push(vec![]);
                        }
                        entry_is_header.push(false); // source header, but selectable
                        entry_dimmed.push(source.error.is_some());
                        entry_indent.push(0);
                        entry_data_indices.push(None);
                        entry_group_keys.push(Some(si.to_string()));
                        if source.error.is_some() {
                            entry_badge_text.push("[error]".into());
                            entry_badge_color.push(Some(theme.accent_error));
                        } else {
                            entry_badge_text.push(String::new());
                            entry_badge_color.push(None);
                        }
                        if collapsed {
                            continue;
                        }
                        for &pi in plugin_order {
                            let plugin = &source.plugins[pi];
                            if !fuzzy_matches(&plugin.name, state.picker_state.query()) {
                                continue;
                            }
                            let status_label = match plugin.install_status.as_str() {
                                "installed" => "[installed]",
                                "update_available" => "[update available]",
                                _ => "",
                            };
                            entry_labels.push(plugin.name.clone());
                            let right = match (plugin.version.as_deref(), plugin.author.as_deref())
                            {
                                (Some(v), Some(a)) => format!("v{v} by {a}"),
                                (Some(v), None) => format!("v{v}"),
                                (None, Some(a)) => format!("by {a}"),
                                (None, None) => String::new(),
                            };
                            entry_right_labels.push(right);
                            let desc = plugin.description.as_deref().unwrap_or("");
                            if desc.is_empty() {
                                entry_desc_lines.push(vec![]);
                            } else {
                                entry_desc_lines.push(vec![desc.to_string()]);
                            }
                            match marketplace_components_summary(plugin) {
                                Some(summary) => entry_summary_lines.push(vec![summary]),
                                None => entry_summary_lines.push(vec![]),
                            }
                            // Fields for expanded view.
                            let mut fields = Vec::new();
                            if let Some(ref version) = plugin.version {
                                fields.push(("version".to_string(), version.clone()));
                            }
                            if let Some(ref author) = plugin.author {
                                fields.push(("author".to_string(), author.clone()));
                            }
                            if let Some(ref category) = plugin.category {
                                fields.push(("category".to_string(), category.clone()));
                            }
                            if !plugin.tags.is_empty() {
                                fields.push(("tags".to_string(), plugin.tags.join(", ")));
                            }
                            match &plugin.components {
                                Some(components) if !components.is_empty() => {
                                    fields.extend(render_components_fields(components));
                                }
                                Some(_) => {
                                    fields.push((
                                        "provides".to_string(),
                                        NO_DETECTABLE_COMPONENTS.to_string(),
                                    ));
                                }
                                None => {
                                    if plugin.remote_url.is_some() {
                                        fields.push((
                                            "provides".to_string(),
                                            "contents shown after install".to_string(),
                                        ));
                                    }
                                }
                            }
                            if plugin.install_status != "not_installed" {
                                fields.push(("status".to_string(), plugin.install_status.clone()));
                                if let Some(ref iv) = plugin.installed_version {
                                    fields.push(("installed".to_string(), iv.clone()));
                                }
                            }
                            entry_fields.push(fields);
                            entry_is_header.push(false);
                            entry_dimmed.push(false); // marketplace items aren't disabled
                            entry_indent.push(1);
                            entry_data_indices.push(Some(si));
                            entry_group_keys.push(None);
                            entry_badge_text.push(status_label.to_string());
                            entry_badge_color.push(match plugin.install_status.as_str() {
                                "installed" => Some(theme.accent_success),
                                "update_available" => Some(theme.warning),
                                _ => None,
                            });
                        }
                    }
                } else if let TabDataState::Error(ref msg) = state.marketplace_data {
                    entry_labels.push(format!("Error: {}", msg));
                    entry_right_labels.push(String::new());
                    entry_desc_lines.push(vec![]);
                    entry_summary_lines.push(vec![]);
                    entry_fields.push(vec![]);
                    entry_is_header.push(false);
                    entry_dimmed.push(false);
                    entry_indent.push(0);
                    entry_data_indices.push(None);
                    entry_group_keys.push(None);
                    entry_badge_text.push(String::new());
                    entry_badge_color.push(None);
                }
            }
            ExtensionsTab::McpServers => {
                if let TabDataState::Loaded(ref servers) = state.mcps_data {
                    use crate::views::mcps_modal::{
                        McpSectionId, section_description_lines, section_for, section_key,
                        section_label,
                    };

                    init_mcps_section_collapse_on_first_load(
                        &mut state.mcps_collapsed_sections,
                        &mut state.mcps_section_collapse_initialized,
                        servers,
                    );

                    let searching = !state.picker_state.query().is_empty();
                    let mut sections: std::collections::BTreeMap<
                        McpSectionId,
                        Vec<(usize, &crate::views::mcps_modal::McpServerInfo)>,
                    > = std::collections::BTreeMap::new();
                    for (si, server) in servers.iter().enumerate() {
                        let display_name = server.display_name.as_deref().unwrap_or(&server.name);
                        if !fuzzy_matches(display_name, state.picker_state.query()) {
                            continue;
                        }
                        if !state.mcps_filter.matches(server.enabled) {
                            continue;
                        }
                        sections
                            .entry(section_for(server))
                            .or_default()
                            .push((si, server));
                    }

                    for (section_id, section_servers) in &sections {
                        let sec_key = section_key(section_id);
                        let section_collapsed = mcp_section_children_hidden(
                            &state.mcps_collapsed_sections,
                            &sec_key,
                            searching,
                        );
                        entry_labels.push(section_label(section_id, section_servers.len()));
                        entry_right_labels.push(String::new());
                        entry_desc_lines.push(section_description_lines(
                            section_id,
                            state.session_team_id.as_deref(),
                        ));
                        entry_summary_lines.push(vec![]);
                        entry_fields.push(vec![]);
                        entry_is_header.push(false);
                        entry_dimmed.push(false);
                        entry_indent.push(0);
                        entry_data_indices.push(None);
                        entry_group_keys.push(Some(sec_key));
                        entry_badge_text.push(String::new());
                        entry_badge_color.push(None);
                        if section_collapsed {
                            continue;
                        }
                        for &(si, server) in section_servers {
                            entry_labels.push(
                                server
                                    .display_name
                                    .clone()
                                    .unwrap_or_else(|| server.name.clone()),
                            );
                            entry_right_labels.push(format!("({})", server.source));
                            // Summary line: tools count + enabled count.
                            if server.tools.is_empty() {
                                entry_desc_lines.push(vec![
                                    "no tools (server may not be connected)".to_string(),
                                ]);
                            } else {
                                let enabled_count =
                                    server.tools.iter().filter(|t| t.enabled).count();
                                if enabled_count == server.tools.len() {
                                    entry_desc_lines
                                        .push(vec![format!("{} tools", server.tools.len())]);
                                } else {
                                    entry_desc_lines.push(vec![format!(
                                        "{} tools ({} enabled)",
                                        server.tools.len(),
                                        enabled_count
                                    )]);
                                }
                            }
                            entry_summary_lines.push(vec![]);
                            entry_fields.push(vec![]);
                            let tools_group_key = format!("mcp-tools:{si}");
                            entry_is_header.push(false);
                            entry_dimmed.push(!server.enabled);
                            entry_indent.push(1);
                            entry_data_indices.push(Some(si));
                            entry_group_keys.push(Some(tools_group_key));
                            let (badge_text, badge_col) = if !server.enabled {
                                ("[disabled]".to_string(), Some(theme.accent_error))
                            } else {
                                (
                                    format!("[{}]", server.status.label()),
                                    Some(server.status.theme_color(&theme)),
                                )
                            };
                            entry_badge_text.push(badge_text);
                            entry_badge_color.push(badge_col);
                            if state.mcps_tools_expanded.contains(&si) {
                                for t in &server.tools {
                                    entry_labels.push(
                                        t.display_name.clone().unwrap_or_else(|| t.name.clone()),
                                    );
                                    entry_right_labels.push(String::new());
                                    let desc = t.description.as_deref().unwrap_or("");
                                    if desc.is_empty() {
                                        entry_desc_lines.push(vec![]);
                                    } else {
                                        entry_desc_lines.push(vec![desc.to_string()]);
                                    }
                                    entry_summary_lines.push(vec![]);
                                    entry_fields.push(vec![]);
                                    entry_is_header.push(false);
                                    entry_dimmed.push(!t.enabled);
                                    entry_indent.push(2);
                                    entry_data_indices.push(Some(si));
                                    entry_group_keys.push(None);
                                    let tool_badge = if !t.enabled {
                                        ("[disabled]".to_string(), Some(theme.accent_error))
                                    } else {
                                        (String::new(), None)
                                    };
                                    entry_badge_text.push(tool_badge.0);
                                    entry_badge_color.push(tool_badge.1);
                                }
                            }
                        }
                    }
                } else if let TabDataState::Error(ref msg) = state.mcps_data {
                    entry_labels.push(format!("Error: {}", msg));
                    entry_right_labels.push(String::new());
                    entry_desc_lines.push(vec![]);
                    entry_summary_lines.push(vec![]);
                    entry_fields.push(vec![]);
                    entry_is_header.push(false);
                    entry_dimmed.push(false);
                    entry_indent.push(0);
                    entry_data_indices.push(None);
                    entry_group_keys.push(None);
                    entry_badge_text.push(String::new());
                    entry_badge_color.push(None);
                }
            }
        }
    }

    // Override badge for the entry with an in-flight action.
    if let Some(pending_idx) = state.pending_entry_index
        && let Some(ref pending_text) = state.pending_action
    {
        if let Some(badge) = entry_badge_text.get_mut(pending_idx) {
            *badge = format!("[{}]", pending_text.trim_end_matches("..."));
        }
        if let Some(color) = entry_badge_color.get_mut(pending_idx) {
            *color = Some(theme.warning);
        }
    }
    // A completed action marks its row with a checkmark (auto-expiring),
    // overriding the in-flight pending badge — keeps the list visible, no
    // overlay. The full result text is shown non-covering in the footer below.
    if let Some(ref n) = state.result_notice
        && let Some(row) = n.entry_index
    {
        if let Some(badge) = entry_badge_text.get_mut(row) {
            *badge = "✓".to_string();
        }
        if let Some(color) = entry_badge_color.get_mut(row) {
            *color = Some(theme.accent_success);
        }
    }
    // Build the PickerField slices from owned data.
    let field_slices: Vec<Vec<picker::PickerField<'_>>> = entry_fields
        .iter()
        .map(|fields| {
            fields
                .iter()
                .map(|(l, v)| picker::PickerField {
                    label: l.as_str(),
                    value: v.as_str(),
                })
                .collect()
        })
        .collect();
    let desc_line_refs: Vec<Vec<&str>> = entry_desc_lines
        .iter()
        .map(|lines| lines.iter().map(|s| s.as_str()).collect())
        .collect();
    let summary_line_refs: Vec<Vec<&str>> = entry_summary_lines
        .iter()
        .map(|lines| lines.iter().map(|s| s.as_str()).collect())
        .collect();

    // Build non_selectable mask (MCP section labels are not focusable rows).
    let non_selectable = build_entry_non_selectable(&entry_is_header, &entry_group_keys);
    let non_selectable_clickable = build_entry_non_selectable_clickable(&entry_group_keys);

    // Min-clamp and skip rows marked non-selectable for this frame's footer +
    // highlight. Do not write to `state` until after `render_modal_window`
    // succeeds — early return would desync `selected` from entry maps
    // published only post-paint.
    let entry_count = entry_labels.len();
    let selected =
        picker::first_selectable_index(state.picker_state.selected, entry_count, &non_selectable);

    // Build per-tab action keys for the footer shortcuts.
    // Space enable/disable uses the freshly built entry-mapping locals
    // (not `state.entry_*`, which are published once after paint below).
    let action_keys = extensions_action_keys(state.active_tab);

    // Build owned labels for dynamic action-key shortcuts so we can
    // borrow from them without leaking memory. Each entry is
    // `(original_index, label)` so the shortcut `id` stays aligned
    // with `action_keys` even when some keys have no display string.
    let action_labels: Vec<(usize, String)> = action_keys
        .iter()
        .enumerate()
        .filter_map(|(i, &(ch, desc))| {
            let key_str = action_key_display(ch);
            if key_str.is_empty() {
                None
            } else {
                let verb = action_key_footer_desc_for_mapping(
                    ch,
                    desc,
                    state,
                    &entry_data_indices,
                    &entry_group_keys,
                    selected,
                );
                Some((i, format!("{key_str} {verb}")))
            }
        })
        .collect();

    // Build Shortcut list for the modal window footer.
    // Standard nav/select/close + expandable hints + per-tab action keys.
    // Build footer shortcuts: per-tab action keys + Esc close.
    // All shortcuts are clickable so they get hover highlights and
    // dispatch actions on click.
    //
    // When a modal message overlay (error OR confirmation) is showing,
    // the standard shortcuts are suppressed and a custom hint is
    // rendered directly into the footer area below. Custom render is
    // used because the dismissal keys ("any key") are multi-word and
    // would not split correctly through the default Shortcut renderer.
    // The overlay above is shortened to leave the footer line visible.
    let modal_msg_kind = state.modal_message.as_ref().map(|m| match m {
        ModalMessage::Error(_) => ModalMsgKind::Error,
        ModalMessage::Confirmation { .. } => ModalMsgKind::Confirm,
    });
    let mut shortcuts: Vec<Shortcut<'_>> = Vec::new();
    if modal_msg_kind.is_some() {
        // Modal message overlay (error/confirmation) is rendered with
        // its own dismissal hint in the footer below — leave the
        // standard shortcuts list empty.
    } else if state.picker_state.search_active && state.input.is_none() && state.mcp_setup.is_none()
    {
        // Search bar has focus — hide the shortcuts footer entirely so
        // it doesn't compete visually with the typing cursor and so
        // typed letters don't appear to map to advertised actions
        // (they're going into the query, not triggering shortcuts).
        // Input-mode is handled below; it owns its own footer.
    } else if state.mcp_setup.is_some() {
        shortcuts.push(Shortcut {
            label: "Enter save and authenticate",
            clickable: false,
            id: 0,
        });
        shortcuts.push(Shortcut {
            label: "↑/↓ select",
            clickable: false,
            id: 0,
        });
        shortcuts.push(Shortcut {
            label: "Esc cancel",
            clickable: false,
            id: 0,
        });
    } else if let Some(ref input) = state.input {
        // "Add"/input mode: surface the keys the input form actually
        // handles. Tab is either path completion (single-field) or
        // field navigation (multi-field).
        shortcuts.push(Shortcut {
            label: "Enter submit",
            clickable: false,
            id: 0,
        });
        shortcuts.push(Shortcut {
            label: if input.is_multi_field() {
                "Tab/Shift+Tab field"
            } else {
                "Tab complete"
            },
            clickable: false,
            id: 0,
        });
        shortcuts.push(Shortcut {
            label: "Esc cancel",
            clickable: false,
            id: 0,
        });
    } else {
        // Tab/Shift+Tab cycles tabs (handled in picker.rs). Click on
        // the hint cycles to the next tab only — sentinel id 98,
        // dispatched in `handle_extensions_modal_mouse`. The hint
        // label intentionally documents only `Tab` because click
        // cycles forward; `Shift+Tab` is still listed in the
        // cheatsheet (`?` shortcut help).
        shortcuts.push(Shortcut {
            label: "Tab tabs",
            clickable: true,
            id: 98,
        });
        for &(orig_idx, ref label) in &action_labels {
            shortcuts.push(Shortcut {
                label: label.as_str(),
                clickable: true,
                id: 100 + orig_idx,
            });
        }
        if state.active_tab == ExtensionsTab::McpServers {
            shortcuts.push(Shortcut {
                label: MCP_SERVERS_OPEN_CONNECTORS_FOOTER,
                clickable: false,
                id: 0,
            });
        }
        // `e` / Shift+e / Enter still expands and collapses (handled by
        // the picker's built-in expandable branch). The hint is omitted
        // from the footer to save space — the cheatsheet still lists it.
        // ID 99 = close action, handled in the mouse handler.
        shortcuts.push(Shortcut {
            label: "Esc close",
            clickable: true,
            id: 99,
        });
        // Surface `i search` in the footer when vim nav mode is active — but
        // only on tabs where `i` is not already an action key (Marketplace
        // `install`, MCP Servers `auth`). `handle_picker_input` resolves action
        // keys before vim search entry, so on those tabs `i` never opens search
        // and the hint would mislabel the key.
        let i_is_action_key = extensions_action_keys(state.active_tab)
            .iter()
            .any(|&(ch, _)| ch == 'i');
        if !i_is_action_key {
            modal_window::push_vim_nav_search_hint(
                &mut shortcuts,
                state.picker_state.search_active,
            );
        }
    }

    // Render modal window chrome.
    let modal_config = ModalWindowConfig {
        // Empty title — the tab bar identifies the modal contents.
        title: "",
        tabs: Some(&labels),
        shortcuts: &shortcuts,
        sizing: ModalSizing {
            width_pct: 0.65,
            max_width: 160,
            min_width: 40,
            v_margin: 3,
            h_pad: 2,
            v_pad: 2,
            footer_lines: 2,
        }
        .with_compact(compact),
        fold_info: None,
    };
    let Some(ModalContentArea {
        content: content_area,
        footer: footer_area,
        inner_x,
        inner_width,
    }) =
        modal_window::render_modal_window(buf, full_area, &mut state.window, &modal_config, &theme)
    else {
        // Too small to paint: leave selection + entry caches unchanged
        // together (see clamp comment above).
        return;
    };

    // Commit the clamped selection now that paint will continue and
    // entry maps will be published later this frame.
    state.picker_state.selected = selected;

    // In input ("Add") mode the search bar, filter indicator, and divider
    // are hidden so the bordered input form can own the full content area.
    // The search affordances are irrelevant while the user is typing into
    // a form, and the divider would float above the form awkwardly.

    let search_width = content_area.width;
    if !in_input_mode {
        // Search bar at top of content area.
        let search_active_render = state.picker_state.search_active;
        picker::render_picker_search_bar(
            buf,
            content_area.x,
            content_area.y,
            search_width,
            &theme,
            &state.picker_state,
            search_active_render,
            true, // show_search_hint
            Some(theme.bg_base),
        );
    }

    // Filter indicator (optional, right-aligned on search row).
    if has_filter && !in_input_mode {
        let rect = picker::render_filter_indicator(
            buf,
            content_area.x,
            content_area.y,
            search_width,
            &theme,
            filter.label(),
            "f",
            filter != StatusFilter::All,
            state.picker_state.filter_hovered,
        );
        state.picker_state.filter_area = Some(rect);
    } else {
        state.picker_state.filter_area = None;
    }

    // Divider below search — spans full inner width (border to border).
    // Suppressed in input mode (no search bar above to divide from).
    let sep_y = content_area.y + 1;
    if !in_input_mode && sep_y < content_area.y + content_area.height {
        picker::render_divider(
            buf,
            inner_x,
            sep_y,
            inner_width,
            &theme,
            Some(theme.bg_base),
        );
    }

    // In input mode the form takes the full content area (no search/divider
    // chrome above). Otherwise entries start one row below the divider.
    let entries_start_y = if in_input_mode {
        content_area.y
    } else {
        sep_y + 1
    };
    // Search-bar hit area: zero-rect in input mode so a click in that
    // (now empty) row doesn't accidentally activate search underneath.
    let search_bar_rect = if in_input_mode {
        Rect::default()
    } else {
        Rect::new(content_area.x, content_area.y, content_area.width, 1)
    };

    // Underline the Managed section's last description line (the connectors URL) as a link affordance.
    let managed_section_key =
        crate::views::mcps_modal::section_key(&crate::views::mcps_modal::McpSectionId::Managed);
    // `underline_last_desc` and the recorded click band both assume the URL is the
    // LAST Managed description line; trip a test if that ever stops holding.
    debug_assert!(
        crate::views::mcps_modal::section_description_lines(
            &crate::views::mcps_modal::McpSectionId::Managed,
            state.session_team_id.as_deref(),
        )
        .last()
        .is_some_and(|l| l.starts_with('[') && l.ends_with(']')),
        "Managed section's last description line must be the bracketed connectors URL",
    );
    let picker_entries: Vec<picker::PickerEntry<'_>> = entry_labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            if entry_is_header[i] {
                picker::PickerEntry::Header {
                    label: label.as_str(),
                }
            } else {
                let group_key = entry_group_keys.get(i).and_then(|k| k.as_ref());
                let is_collapsible = group_key.is_some();
                // For collapsible group headers, `expanded` reflects
                // whether the group's children are visible (not in the
                // collapsed set). For regular items, it reflects the
                // picker's per-row detail expansion.
                let is_expanded = if is_collapsible {
                    state.is_group_expanded(i, group_key.unwrap())
                } else {
                    state.picker_state.expanded.contains(&i)
                };
                picker::PickerEntry::Row(picker::PickerRow {
                    label: label.as_str(),
                    right_label: entry_right_labels[i].as_str(),
                    selected: !state.picker_state.search_active
                        && !state.picker_state.tabs_focused
                        && i == state.picker_state.selected,
                    expanded: is_expanded,
                    fields: &field_slices[i],
                    description_lines: &desc_line_refs[i],
                    summary_lines: &summary_line_refs[i],
                    dimmed: entry_dimmed.get(i).copied().unwrap_or(false),
                    indent: entry_indent.get(i).copied().unwrap_or(0),
                    badge: entry_badge_text.get(i).map(|s| s.as_str()).unwrap_or(""),
                    badge_color: entry_badge_color.get(i).copied().flatten(),
                    collapsible: is_collapsible,
                    underline_last_desc: state.modal_message.is_none()
                        && group_key.is_some_and(|k| *k == managed_section_key),
                })
            }
        })
        .collect();

    // Render entries into the area below the divider.
    let entries_area = Rect {
        x: content_area.x,
        y: entries_start_y,
        width: content_area.width,
        height: content_area
            .height
            .saturating_sub(entries_start_y.saturating_sub(content_area.y)),
    };
    // In input mode the entry list is intentionally empty (we skip the
    // entry-builder loop above). Calling the picker here would render
    // its empty-state "No matches" message, which is misleading — there
    // are no entries because we're showing a form, not because nothing
    // matched. Skip the picker render and let the input-form overlay
    // below own the entries area instead.
    let (item_rects, entry_indices) = if in_input_mode {
        // No picker render in input mode: clear any stale recorded link band.
        state.picker_state.link_band = None;
        (Vec::new(), Vec::new())
    } else {
        let content_hit = picker::render_picker_content_with_scrollbar_x(
            buf,
            entries_area,
            &theme,
            &mut state.picker_state,
            &picker_entries,
            &non_selectable,
            &non_selectable_clickable,
            Some(theme.bg_base),
            loading,
            0,
            inner_x + inner_width - 1,
        );
        (content_hit.item_rects, content_hit.entry_indices)
    };

    // Store hit areas for mouse handling. Build a PickerHitAreas so
    // handle_picker_input (used for content-level events) works.
    let filter_rect = state.picker_state.filter_area;
    state.picker_state.hit_areas = Some(picker::PickerHitAreas {
        close_button: Rect::default(), // handled by ModalWindow
        search_bar: search_bar_rect,
        item_rects,
        entry_indices,
        tab_rects: vec![], // handled by ModalWindow
        filter_rect,
    });
    state.entry_data_indices = entry_data_indices;
    state.entry_labels_cache = entry_labels;
    state.entry_group_keys = entry_group_keys;
    state.entry_non_selectable = non_selectable;
    state.entry_non_selectable_clickable = non_selectable_clickable;

    // Render input form overlay (when in input mode).
    if let Some(ref setup) = state.mcp_setup {
        let form_y = entries_start_y;
        let form_height = entries_area.height;
        if form_height > 0 {
            let form_area = Rect::new(content_area.x, form_y, content_area.width, form_height);
            render_mcp_setup_form(buf, form_area, setup, &theme);
        }
    } else if let Some(ref input) = state.input {
        let form_y = entries_start_y;
        let form_height = entries_area.height;
        if form_height > 0 {
            let form_area = Rect::new(content_area.x, form_y, content_area.width, form_height);
            render_input_form(buf, form_area, input, &theme);
        }
    }

    // Render full-screen pending overlay when no specific entry is targeted
    // (e.g., AddSource — the new row doesn't exist yet so there's no entry
    // badge to show). Covers the picker content with a centered spinner + message.
    if state.pending_action.is_some()
        && state.pending_entry_index.is_none()
        && let Some(popup_rect) = state.window.popup_area
    {
        let label = state.pending_action.as_deref().unwrap_or("Processing...");
        let frames = crate::glyphs::braille_spinner_frames();
        let frame_idx = (tick / SPINNER_DIVISOR) as usize % frames.len();
        let display = format!("{} {label}", frames[frame_idx]);
        let msg_content_y = popup_rect.y + 2;
        let popup_bottom = popup_rect.y + popup_rect.height.saturating_sub(1);
        let msg_content_height = popup_bottom.saturating_sub(msg_content_y);
        let msg_content_x = popup_rect.x + 1;
        let msg_content_width = popup_rect.width.saturating_sub(2);
        if msg_content_height > 0 {
            let msg_area = Rect::new(
                msg_content_x,
                msg_content_y,
                msg_content_width,
                msg_content_height,
            );
            // Buffer::set_string merges styles; Style::reset clears UNDERLINED/BOLD
            // left by the list underneath (e.g. Managed connectors URL).
            let clear_style = Style::reset().bg(theme.bg_base);
            let text_style = Style::reset().fg(theme.accent_tool).bg(theme.bg_base);
            for y in msg_area.y..msg_area.y + msg_area.height {
                buf.set_string(
                    msg_area.x,
                    y,
                    " ".repeat(msg_area.width as usize),
                    clear_style,
                );
            }
            let msg_y = msg_area.y + msg_area.height / 2;
            let msg_x = msg_area.x + msg_area.width.saturating_sub(display.width() as u16) / 2;
            buf.set_string(msg_x, msg_y, &display, text_style);
        }
    }

    // Render modal message overlay.
    if let Some(ref msg) = state.modal_message {
        let (text, fg) = match msg {
            ModalMessage::Error(e) => (e.as_str(), theme.accent_error),
            ModalMessage::Confirmation { message, .. } => (message.as_str(), theme.accent_tool),
        };
        if let Some(popup_rect) = state.window.popup_area {
            let msg_content_y = popup_rect.y + 2;
            // Stop the overlay above the footer so the dismissal hint
            // we render into the footer below stays visible. Applies to
            // both errors and confirmations.
            let popup_bottom = footer_area.y;
            let msg_content_height = popup_bottom.saturating_sub(msg_content_y);
            let msg_content_x = popup_rect.x + 1;
            let msg_content_width = popup_rect.width.saturating_sub(2);
            if msg_content_height > 0 {
                let msg_area = Rect::new(
                    msg_content_x,
                    msg_content_y,
                    msg_content_width,
                    msg_content_height,
                );
                let clear_style = Style::reset().bg(theme.bg_base);
                let text_style = Style::reset().fg(fg).bg(theme.bg_base);
                for y in msg_area.y..msg_area.y + msg_area.height {
                    buf.set_string(
                        msg_area.x,
                        y,
                        " ".repeat(msg_area.width as usize),
                        clear_style,
                    );
                }
                let pad = 2u16;
                let max_w = msg_area.width.saturating_sub(pad * 2) as usize;
                let wrapped_lines: Vec<&str> = word_wrap(text, max_w);
                let msg_height = wrapped_lines.len().min(msg_area.height as usize);
                let msg_y = msg_area.y + (msg_area.height.saturating_sub(msg_height as u16)) / 2;
                for (i, wline) in wrapped_lines.iter().enumerate().take(msg_height) {
                    buf.set_string(msg_area.x + pad, msg_y + i as u16, wline, text_style);
                }
                // Dismissal hints (for both errors and confirmations)
                // are rendered into the footer below, not inline.
            }
        }
    }

    // Render the dismissal hint(s) for any modal message into the
    // footer area we kept clear above. Custom render (not via Shortcut)
    // is needed because dismissal keys ("any key") are multi-word and
    // would not split correctly through the default renderer. Colors,
    // bold modifier, and "  |  " separator all match the standard
    // footer shortcut style.
    if let Some(kind) = modal_msg_kind {
        let segments: &[(&str, &str)] = match kind {
            ModalMsgKind::Error => &[("any key", " back")],
            ModalMsgKind::Confirm => &[("y", " confirm"), ("any other key", " cancel")],
        };
        render_footer_hint_segments(buf, footer_area, segments, &theme);
    } else if let Some(ref n) = state.result_notice
        && footer_area.height > 0
    {
        // Result status line (per-row and tab-wide): a non-covering success line
        // in the footer so the list stays visible above it. Auto-expires; the
        // per-row case also gets a ✓ on its row.
        let text = n.message.lines().next().unwrap_or(n.message.as_str());
        let avail = footer_area.width.saturating_sub(2) as usize;
        let shown: String = if UnicodeWidthStr::width(text) > avail {
            // Truncate by display width (file convention) so wide chars can't
            // overflow the footer, then add an ellipsis.
            let mut s = String::new();
            let mut w = 0usize;
            for ch in text.chars() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if w + cw > avail.saturating_sub(1) {
                    break;
                }
                w += cw;
                s.push(ch);
            }
            s.push('…');
            s
        } else {
            text.to_string()
        };
        let y = footer_area.y + footer_area.height.saturating_sub(1);
        // The shortcuts bar renders underneath this row and its keys are BOLD;
        // `set_string` only *merges* style, so `Style::default()` would leave
        // that bold on any cell the message overwrites (partial-bold bleed).
        // `Style::reset()` clears all existing modifiers first.
        let clear_style = Style::reset().bg(theme.bg_base);
        let text_style = Style::reset().fg(theme.accent_success).bg(theme.bg_base);
        buf.set_string(
            footer_area.x,
            y,
            " ".repeat(footer_area.width as usize),
            clear_style,
        );
        buf.set_string(footer_area.x + 1, y, &shown, text_style);
    }
}

fn render_mcp_setup_form(buf: &mut Buffer, area: Rect, setup: &McpSetupFormState, theme: &Theme) {
    if area.height < 6 || area.width < 20 {
        return;
    }
    let h_inset: u16 = 2;
    let x = area.x + h_inset;
    let w = area.width.saturating_sub(h_inset * 2);
    let rows = (setup.field.options.len() as u16).saturating_add(4);
    let top = area.y + area.height.saturating_sub(rows) / 2;
    let title = format!("{} · {}", setup.server_name, setup.field.label);
    buf.set_string(
        x,
        top,
        take_by_width(&title, w as usize),
        Style::default()
            .fg(theme.accent_user)
            .bg(theme.bg_base)
            .add_modifier(Modifier::BOLD),
    );
    let hint = "Save and authenticate";
    buf.set_string(
        x,
        top.saturating_add(1),
        hint,
        Style::default().fg(theme.gray).bg(theme.bg_base),
    );
    for (idx, option) in setup.field.options.iter().enumerate() {
        let y = top.saturating_add(3).saturating_add(idx as u16);
        if y >= area.y + area.height {
            break;
        }
        let selected = idx == setup.selected;
        let marker = if selected { "❯" } else { " " };
        let label = format!("{marker} {}", option.label);
        let style = if selected {
            Style::default()
                .fg(theme.text_primary)
                .bg(theme.bg_highlight)
        } else {
            Style::default().fg(theme.text_primary).bg(theme.bg_base)
        };
        buf.set_string(x, y, " ".repeat(w as usize), style);
        buf.set_string(x, y, take_by_width(&label, w as usize), style);
    }
    if let Some(ref err) = setup.error {
        let y = area.y + area.height.saturating_sub(1);
        buf.set_string(
            x,
            y,
            take_by_width(err, w as usize),
            Style::default().fg(theme.accent_error).bg(theme.bg_base),
        );
    }
}

/// Kind of modal message overlay currently showing.
#[derive(Debug, Clone, Copy)]
enum ModalMsgKind {
    Error,
    Confirm,
}

/// Render a centered list of (key, label) hint segments into the bottom
/// row of `footer_area`, joined by `  |  ` separators. Mirrors the
/// styling used by `modal_window::render_modal_shortcuts` so custom
/// dismissal hints look the same as standard footer shortcuts.
fn render_footer_hint_segments(
    buf: &mut Buffer,
    footer_area: Rect,
    segments: &[(&str, &str)],
    theme: &Theme,
) {
    if footer_area.width == 0 || footer_area.height == 0 || segments.is_empty() {
        return;
    }
    let separator = "  |  ";
    let sep_w = separator.width() as u16;

    let mut total_w: u16 = 0;
    for (i, (key, label)) in segments.iter().enumerate() {
        if i > 0 {
            total_w = total_w.saturating_add(sep_w);
        }
        total_w = total_w.saturating_add(key.width() as u16);
        total_w = total_w.saturating_add(label.width() as u16);
    }
    if total_w == 0 || total_w > footer_area.width {
        return;
    }

    let key_style = Style::default()
        .fg(theme.text_secondary)
        .bg(theme.bg_base)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let label_style = Style::default().fg(theme.gray).bg(theme.bg_base);
    let sep_style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);

    let mut x = footer_area.x + footer_area.width.saturating_sub(total_w) / 2;
    let y = footer_area.y + footer_area.height.saturating_sub(1);
    for (i, (key, label)) in segments.iter().enumerate() {
        if i > 0 {
            buf.set_string(x, y, separator, sep_style);
            x += sep_w;
        }
        let kw = key.width() as u16;
        buf.set_string(x, y, *key, key_style);
        x += kw;
        if !label.is_empty() {
            buf.set_string(x, y, *label, label_style);
            x += label.width() as u16;
        }
    }
}

/// Render an inline input form for commands that need arguments.
///
/// Each field gets its own rounded border around the input row,
/// mirroring the prompt input chrome. Labels sit above the bordered
/// row so they remain visible.
///
/// Layout (stacked, one field shown):
/// ```text
///   Label
///   ╭──────────────────────────╮
///   │ ❯ user input             │
///   ╰──────────────────────────╯
/// ```
fn render_input_form(buf: &mut Buffer, area: Rect, input: &ModalInput, theme: &Theme) {
    if area.height < 4 || area.width < 20 {
        return;
    }

    // Per field: 1 label row + 3 rows for the bordered input
    // (top border + content + bottom border).
    let field_count = input.fields().len() as u16;
    const ROWS_PER_FIELD: u16 = 4;
    let separators = field_count.saturating_sub(1); // 1 blank row between fields
    let form_rows = field_count * ROWS_PER_FIELD + separators;
    // Reserve room for an inline error row when present (1 spacer + 1 line).
    let error_rows: u16 = if input.error.is_some() { 2 } else { 0 };
    let total_rows = form_rows + error_rows;

    // Center vertically within the available area.
    let form_top = area.y + area.height.saturating_sub(total_rows) / 2;
    // Horizontal inset so the bordered box doesn't sit flush against
    // the outer modal border.
    let h_inset: u16 = 2;
    let box_x = area.x + h_inset;
    let box_w = area.width.saturating_sub(h_inset * 2);
    if box_w < 10 {
        return;
    }
    let label_x = box_x;

    let label_style = Style::default()
        .fg(theme.accent_user)
        .bg(theme.bg_base)
        .add_modifier(Modifier::BOLD);
    let label_dim_style = Style::default()
        .fg(theme.gray_dim)
        .bg(theme.bg_base)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(theme.text_primary).bg(theme.bg_base);
    let placeholder_style = Style::default().fg(theme.gray).bg(theme.bg_base);
    let prompt_style = Style::default().fg(theme.gray).bg(theme.bg_base);
    let prompt_prefix = crate::glyphs::prompt_arrow();
    let prompt_w = prompt_prefix.width() as u16;

    let mut cur_y = form_top;
    for (fi, field) in input.fields().iter().enumerate() {
        if cur_y >= area.y + area.height {
            break;
        }

        let is_focused = fi == input.focused_index();

        // Row 1: Label (sits above the bordered input, not inside).
        let ls = if is_focused {
            label_style
        } else {
            label_dim_style
        };
        buf.set_string(label_x, cur_y, field.label(), ls);
        cur_y += 1;

        // Rows 2-4: Rounded border around the single-line input.
        // Need at least 3 rows to fit top/content/bottom borders.
        let remaining = (area.y + area.height).saturating_sub(cur_y);
        if remaining < 3 {
            break;
        }
        let box_rect = Rect::new(box_x, cur_y, box_w, 3);
        let border_color = if is_focused {
            theme.prompt_border_active
        } else {
            theme.prompt_border
        };
        let border_style = Style::default().fg(border_color).bg(theme.bg_base);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(border_style)
            .style(Style::default().bg(theme.bg_base));
        let inner = block.inner(box_rect);
        ratatui::widgets::Widget::render(block, box_rect, buf);

        // Content row: prompt prefix + input text or placeholder.
        // Pad 1 cell from the left side of the bordered inner area.
        let content_x = inner.x + 1;
        let content_y = inner.y;
        // Available text width = inner width - left pad - prompt prefix - 1 right margin.
        let max_text_w = inner.width.saturating_sub(1 + prompt_w + 1).max(1) as usize;

        buf.set_string(content_x, content_y, prompt_prefix, prompt_style);
        let text_x = content_x + prompt_w;

        if field.text().is_empty() {
            // Placeholder only renders when the field is NOT focused —
            // matches the prompt widget convention so the cursor isn't
            // overlapping placeholder text on the active row.
            if !is_focused && let Some(ph) = field.placeholder() {
                let display: String = take_by_width(ph, max_text_w);
                buf.set_string(text_x, content_y, &display, placeholder_style);
            }
            if is_focused && let Some(cell) = buf.cell_mut((text_x, content_y)) {
                cell.set_style(Style::default().fg(theme.bg_base).bg(theme.text_primary));
            }
        } else {
            let viewport = field.viewport(max_text_w);
            let visible = &field.text()[viewport.visible_byte_range];
            buf.set_string(text_x, content_y, visible, text_style);

            if is_focused {
                let cx = text_x + viewport.cursor_display_column as u16;
                if cx < inner.x + inner.width
                    && let Some(cell) = buf.cell_mut((cx, content_y))
                {
                    cell.set_style(Style::default().fg(theme.bg_base).bg(theme.text_primary));
                }
            }
        }

        cur_y += 3; // top border + content + bottom border

        // Blank separator between fields (skip after last field).
        if fi + 1 < input.fields().len() {
            cur_y += 1;
        }
    }

    // Inline error message below the form (outside the field borders).
    if let Some(ref err) = input.error {
        cur_y += 1;
        if cur_y < area.y + area.height {
            let error_style = Style::default().fg(theme.accent_error).bg(theme.bg_base);
            let display = take_by_width(err, box_w.saturating_sub(2) as usize);
            buf.set_string(label_x, cur_y, &display, error_style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn derive_source_label_detects_project_scoped_plugins() {
        // Regression: project-scoped `{cwd}/.echo-agent/plugins/<name>/` must label as
        // a (non-removable) plugin, not a removable "Custom" source. The user
        // echo-agent-home branch is ECHO_AGENT_HOME-aware; this covers the project fallback.
        let (label, is_custom) =
            derive_source_label("/repo/work/.echo-agent/plugins/my-plugin/hooks");
        assert_eq!(label, "Plugin: my-plugin");
        assert!(!is_custom);

        let (label, is_custom) =
            derive_source_label("/repo/work/.echo-agent/installed-plugins/vendor-abc123/skills");
        assert_eq!(label, "Plugin: vendor-abc123");
        assert!(!is_custom);
    }

    #[test]
    fn build_entry_non_selectable_leaves_mcp_section_headers_selectable() {
        let mask = build_entry_non_selectable(
            &[false, false, true],
            &[
                Some("mcp-section:managed".into()),
                Some("mcp-tools:0".into()),
                None,
            ],
        );
        assert!(
            !mask[0],
            "MCP section label row is keyboard-selectable so j/k can land on it \
             and Enter / l toggles its collapsed state"
        );
        assert!(!mask[1]);
        assert!(mask[2], "static header rows stay non-selectable");
    }

    #[test]
    fn build_entry_non_selectable_clickable_is_empty_for_mcp_sections() {
        let mask = build_entry_non_selectable_clickable(&[
            Some("mcp-section:managed".into()),
            Some("mcp-tools:0".into()),
            None,
        ]);
        // Sections are now keyboard-selectable, so clicks go through the
        // normal Selected → toggle_fold path; no row needs the
        // non-selectable-but-clickable treatment.
        assert_eq!(mask, vec![false, false, false]);
    }

    #[test]
    fn mcp_servers_action_keys_have_resolver_arms() {
        for &(ch, label) in MCP_SERVERS_ACTION_KEYS {
            let action = resolve_key(ExtensionsTab::McpServers, ch);
            assert!(
                action.is_some(),
                "MCP_SERVERS_ACTION_KEYS advertises ('{ch}', \"{label}\") in the hint bar \
                 but resolve_key(McpServers, '{ch}') returns None — add a match arm in \
                 resolve_key or remove the entry from the const."
            );
        }
    }

    #[test]
    fn action_keys_resolve_and_have_pinned_telemetry_labels() {
        let expected: &[(ExtensionsTab, &[(char, &str)])] = &[
            (
                ExtensionsTab::Hooks,
                &[
                    ('r', "reload"),
                    ('a', "add"),
                    (' ', "toggle"),
                    ('x', "remove"),
                ],
            ),
            (
                ExtensionsTab::Plugins,
                &[
                    ('r', "reload"),
                    ('u', "update"),
                    ('a', "install"),
                    (' ', "toggle"),
                    ('x', "uninstall"),
                ],
            ),
            (
                ExtensionsTab::Marketplace,
                &[
                    ('i', "install"),
                    ('r', "refresh"),
                    ('u', "update"),
                    ('a', "add_source"),
                    ('d', "uninstall"),
                    ('x', "remove_source"),
                ],
            ),
            (
                ExtensionsTab::Skills,
                &[(' ', "toggle"), ('f', "filter"), ('r', "reload")],
            ),
            (ExtensionsTab::Workflows, &[('r', "reload")]),
            (
                ExtensionsTab::McpServers,
                &[
                    ('r', "refresh"),
                    ('a', "add"),
                    ('i', "auth"),
                    (' ', "toggle"),
                    ('x', "remove"),
                ],
            ),
        ];
        assert_eq!(expected.len(), ExtensionsTab::ALL.len());
        for &(tab, pairs) in expected {
            let keys = extensions_action_keys(tab);
            assert_eq!(
                keys.len(),
                pairs.len(),
                "{tab:?}: action key set changed — telemetry `action` values are wire \
                 contract; update this pinning test deliberately"
            );
            for &(ch, label) in pairs {
                assert!(
                    resolve_key(tab, ch).is_some(),
                    "extensions_action_keys({tab:?}) advertises '{ch}' but resolve_key \
                     returns None"
                );
                assert_eq!(
                    action_telemetry_label(tab, ch).as_deref(),
                    Some(label),
                    "telemetry label for ({tab:?}, '{ch}') drifted — these values feed \
                     product analytics; renaming the footer hint renames the metric"
                );
            }
        }
    }

    // Fixture layout (managed section, two servers with tools):
    //   0  section header     group_key=Some("mcp-section:managed")  data=None
    //   1  server 0 header    group_key=Some("mcp-tools:0")          data=Some(0)
    //   2  tool 0 of svr 0    group_key=None                         data=Some(0)
    //   3  tool 1 of svr 0    group_key=None                         data=Some(0)
    //   4  server 1 header    group_key=Some("mcp-tools:1")          data=Some(1)
    //   5  tool 0 of svr 1    group_key=None                         data=Some(1)
    fn fixture_with_two_servers_and_tools() -> ExtensionsModalState {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.entry_data_indices = vec![None, Some(0), Some(0), Some(0), Some(1), Some(1)];
        state.entry_group_keys = vec![
            Some("mcp-section:managed".to_string()),
            Some("mcp-tools:0".to_string()),
            None,
            None,
            Some("mcp-tools:1".to_string()),
            None,
        ];
        state
    }

    #[test]
    fn mcp_setup_form_defaults_and_pref_value() {
        use crate::views::mcps_modal::{
            McpServerDisplayStatus, McpServerInfo, McpSetupConfig, McpSetupField, McpSetupOption,
            McpWireSource,
        };

        let mut server = McpServerInfo {
            name: "acme".into(),
            display_name: None,
            status: McpServerDisplayStatus::SetupRequired,
            tool_count: 0,
            auth_required: false,
            setup_required: true,
            setup: Some(McpSetupConfig {
                fields: vec![McpSetupField {
                    id: "site".into(),
                    label: "Site".into(),
                    field_type: "select".into(),
                    required: true,
                    default: Some("us1".into()),
                    options: vec![
                        McpSetupOption {
                            label: "US1".into(),
                            value: "us1".into(),
                        },
                        McpSetupOption {
                            label: "US5".into(),
                            value: "us5".into(),
                        },
                    ],
                }],
            }),
            setup_values: std::collections::HashMap::new(),
            tools: vec![],
            enabled: true,
            source: "plugin: acme".into(),
            wire_source: McpWireSource::Local,
            plugin_name: Some("acme".into()),
            is_managed_gateway: false,
        };
        let form = McpSetupFormState::new(&server).unwrap();
        assert_eq!(form.selected_value().as_deref(), Some("us1"));
        server.setup_values.insert("site".into(), "us5".into());
        let form = McpSetupFormState::new(&server).unwrap();
        assert_eq!(form.selected_value().as_deref(), Some("us5"));
        assert_eq!(form.values().unwrap()["site"], "us5");
    }

    #[test]
    fn selected_mcp_tool_returns_none_on_server_row() {
        let mut state = fixture_with_two_servers_and_tools();
        state.picker_state.selected = 0;
        assert_eq!(state.selected_mcp_tool(), None);
        state.picker_state.selected = 1;
        assert_eq!(state.selected_mcp_tool(), None);
        state.picker_state.selected = 4;
        assert_eq!(state.selected_mcp_tool(), None);
    }

    #[test]
    fn selected_mcp_tool_returns_tool_index_on_tool_row() {
        let mut state = fixture_with_two_servers_and_tools();
        state.picker_state.selected = 2;
        assert_eq!(state.selected_mcp_tool(), Some((0, 0)));
        state.picker_state.selected = 3;
        assert_eq!(state.selected_mcp_tool(), Some((0, 1)));
        state.picker_state.selected = 5;
        assert_eq!(state.selected_mcp_tool(), Some((1, 0)));
    }

    #[test]
    fn mcp_section_managed_collapsed_hides_child_servers() {
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert("mcp-section:managed".to_string());
        assert!(mcp_section_children_hidden(
            &collapsed,
            "mcp-section:managed",
            false
        ));
    }

    #[test]
    fn mcp_section_search_forces_children_visible() {
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert("mcp-section:managed".to_string());
        assert!(!mcp_section_children_hidden(
            &collapsed,
            "mcp-section:managed",
            true
        ));
    }

    #[test]
    fn mcp_is_group_expanded_search_overrides_collapsed_section() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state
            .mcps_collapsed_sections
            .insert("mcp-section:managed".to_string());
        state.picker_state.set_query("linear");
        assert!(state.is_group_expanded(0, "mcp-section:managed"));
    }

    #[test]
    fn mcp_tools_is_group_expanded_follows_mcps_tools_expanded() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        assert!(!state.is_group_expanded(1, "mcp-tools:0"));
        state.mcps_tools_expanded.insert(0);
        assert!(state.is_group_expanded(1, "mcp-tools:0"));
        state.mcps_tools_expanded.remove(&0);
        assert!(!state.is_group_expanded(1, "mcp-tools:0"));
    }

    #[test]
    fn mcp_auth_intercept_on_expand_detects_auth_required_server() {
        use crate::views::mcps_modal::{McpServerDisplayStatus, McpServerInfo, McpWireSource};

        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.mcps_data = TabDataState::Loaded(vec![McpServerInfo {
            name: "needs-oauth".into(),
            display_name: None,
            status: McpServerDisplayStatus::NeedsAuth,
            tool_count: 0,
            auth_required: true,
            setup_required: false,
            setup: None,
            setup_values: std::collections::HashMap::new(),
            tools: vec![],
            enabled: true,
            source: "managed".into(),
            wire_source: McpWireSource::Managed,
            plugin_name: None,
            is_managed_gateway: false,
        }]);
        state.entry_data_indices = vec![None, Some(0)];
        state.entry_group_keys = vec![
            Some("mcp-section:managed".into()),
            Some("mcp-tools:0".into()),
        ];
        state.picker_state.selected = 1;
        assert!(
            state.mcp_auth_intercept_on_expand(),
            "collapsed auth-required server should intercept expand"
        );
        state.mcps_tools_expanded.insert(0);
        assert!(
            !state.mcp_auth_intercept_on_expand(),
            "expanded tools must not intercept (collapse allowed)"
        );
        state.picker_state.selected = 0;
        assert!(!state.mcp_auth_intercept_on_expand());
    }

    fn make_mcp_server_for_rows(
        name: &str,
        wire: crate::views::mcps_modal::McpWireSource,
        tools: Vec<(&str, bool)>,
    ) -> crate::views::mcps_modal::McpServerInfo {
        use crate::views::mcps_modal::{McpServerDisplayStatus, McpToolDetail};
        let tool_details: Vec<McpToolDetail> = tools
            .into_iter()
            .map(|(n, enabled)| McpToolDetail {
                name: n.into(),
                display_name: None,
                description: None,
                enabled,
            })
            .collect();
        let tc = tool_details.len();
        crate::views::mcps_modal::McpServerInfo {
            name: name.into(),
            display_name: None,
            status: McpServerDisplayStatus::Ready,
            tool_count: tc,
            auth_required: false,
            setup_required: false,
            setup: None,
            setup_values: std::collections::HashMap::new(),
            tools: tool_details,
            enabled: true,
            source: "local".into(),
            wire_source: wire,
            plugin_name: None,
            is_managed_gateway: false,
        }
    }

    #[test]
    fn mcp_collapsed_managed_section_omits_server_rows() {
        use crate::views::mcps_modal::McpWireSource;

        let servers = vec![
            make_mcp_server_for_rows("echo_agent_com_linear", McpWireSource::Managed, vec![]),
            make_mcp_server_for_rows("local-srv", McpWireSource::Local, vec![]),
        ];
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert("mcp-section:managed".to_string());
        let rows = build_mcp_servers_picker_rows(
            &servers,
            "",
            StatusFilter::All,
            &collapsed,
            &std::collections::HashSet::new(),
        );
        assert!(
            rows.labels
                .iter()
                .any(|l| l.starts_with("Managed by cloud.echo-agent.invalid")),
            "managed section header must appear"
        );
        assert!(
            !rows.labels.iter().any(|l| l == "echo_agent_com_linear"),
            "servers in collapsed managed section must be omitted"
        );
        assert!(
            rows.labels.iter().any(|l| l.starts_with("Local")),
            "local section should still render"
        );
        assert!(rows.labels.iter().any(|l| l == "local-srv"));
    }

    #[test]
    fn mcp_tool_rows_emitted_when_tools_expanded_by_server_index() {
        use crate::views::mcps_modal::McpWireSource;

        let servers = vec![
            make_mcp_server_for_rows(
                "alpha",
                McpWireSource::Managed,
                vec![("tool-a1", true), ("tool-a2", true)],
            ),
            make_mcp_server_for_rows("beta", McpWireSource::Managed, vec![("tool-b1", true)]),
        ];
        let mut tools_expanded = std::collections::HashSet::new();
        tools_expanded.insert(0);
        let rows = build_mcp_servers_picker_rows(
            &servers,
            "",
            StatusFilter::All,
            &std::collections::HashSet::new(),
            &tools_expanded,
        );
        assert!(rows.labels.contains(&"tool-a1".to_string()));
        assert!(rows.labels.contains(&"tool-a2".to_string()));
        assert!(
            !rows.labels.contains(&"tool-b1".to_string()),
            "only expanded server index 0 should show tools"
        );
        let tail_index = rows.labels.len().saturating_sub(1);
        assert_ne!(
            tools_expanded.iter().copied().next(),
            Some(tail_index),
            "expansion must not use picker tail index"
        );
    }

    #[test]
    fn mcps_plugin_sections_collapsed_on_first_load() {
        use crate::views::mcps_modal::{McpServerDisplayStatus, McpServerInfo, McpWireSource};

        let servers = vec![
            McpServerInfo {
                name: "p1-srv".into(),
                display_name: None,
                status: McpServerDisplayStatus::Ready,
                tool_count: 0,
                auth_required: false,
                setup_required: false,
                setup: None,
                setup_values: std::collections::HashMap::new(),
                tools: vec![],
                enabled: true,
                source: "plugin: alpha".into(),
                wire_source: McpWireSource::Local,
                plugin_name: Some("alpha".into()),
                is_managed_gateway: false,
            },
            McpServerInfo {
                name: "p2-srv".into(),
                display_name: None,
                status: McpServerDisplayStatus::Ready,
                tool_count: 0,
                auth_required: false,
                setup_required: false,
                setup: None,
                setup_values: std::collections::HashMap::new(),
                tools: vec![],
                enabled: true,
                source: "plugin: beta".into(),
                wire_source: McpWireSource::Local,
                plugin_name: Some("beta".into()),
                is_managed_gateway: false,
            },
        ];
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        assert!(
            !state.mcps_collapsed_sections.contains("mcp-section:local"),
            "Local section starts expanded by default for a less noisy initial view"
        );
        assert!(
            !state
                .mcps_collapsed_sections
                .contains("mcp-section:managed")
        );
        init_mcps_section_collapse_on_first_load(
            &mut state.mcps_collapsed_sections,
            &mut state.mcps_section_collapse_initialized,
            &servers,
        );
        assert!(
            state
                .mcps_collapsed_sections
                .contains("mcp-section:plugin:alpha")
        );
        assert!(
            state
                .mcps_collapsed_sections
                .contains("mcp-section:plugin:beta")
        );
        assert!(state.mcps_section_collapse_initialized);
    }

    #[test]
    fn seed_mcps_section_collapse_for_cta_expands_only_target() {
        use crate::views::mcps_modal::{McpServerDisplayStatus, McpServerInfo, McpWireSource};

        let server = |name: &str, plugin: Option<&str>| McpServerInfo {
            name: name.into(),
            display_name: None,
            status: McpServerDisplayStatus::Ready,
            tool_count: 0,
            auth_required: false,
            setup_required: false,
            setup: None,
            setup_values: std::collections::HashMap::new(),
            tools: vec![],
            enabled: true,
            source: plugin
                .map(|p| format!("plugin: {p}"))
                .unwrap_or_else(|| "local".into()),
            wire_source: McpWireSource::Local,
            plugin_name: plugin.map(str::to_string),
            is_managed_gateway: false,
        };
        let servers = vec![
            server("echo_agent_com_x", None),
            server("local-srv", None),
            server("alpha-srv", Some("alpha")),
            server("beta-srv", Some("beta")),
        ];
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        seed_mcps_section_collapse_for_cta(
            &mut state.mcps_collapsed_sections,
            &mut state.mcps_section_collapse_initialized,
            &servers,
            "alpha",
        );
        // Managed AND Local are collapsed too (not just other plugins).
        assert!(
            state
                .mcps_collapsed_sections
                .contains("mcp-section:managed")
        );
        assert!(state.mcps_collapsed_sections.contains("mcp-section:local"));
        assert!(
            state
                .mcps_collapsed_sections
                .contains("mcp-section:plugin:beta")
        );
        // Only the target plugin stays expanded.
        assert!(
            !state
                .mcps_collapsed_sections
                .contains("mcp-section:plugin:alpha")
        );
        // Default first-load seeder will no-op.
        assert!(state.mcps_section_collapse_initialized);
        init_mcps_section_collapse_on_first_load(
            &mut state.mcps_collapsed_sections,
            &mut state.mcps_section_collapse_initialized,
            &servers,
        );
        assert!(
            !state
                .mcps_collapsed_sections
                .contains("mcp-section:plugin:alpha")
        );
    }

    #[test]
    fn selected_mcp_tool_returns_none_on_empty_or_out_of_range() {
        let state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        assert_eq!(state.selected_mcp_tool(), None);
        let mut state = fixture_with_two_servers_and_tools();
        state.picker_state.selected = 99;
        assert_eq!(state.selected_mcp_tool(), None);
    }

    #[test]
    fn selected_mcp_tool_returns_none_on_error_or_loading_entry() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.entry_data_indices = vec![None];
        state.entry_group_keys = vec![None];
        state.picker_state.selected = 0;
        assert_eq!(state.selected_mcp_tool(), None);
    }

    #[test]
    fn selected_mcp_tool_returns_none_when_no_parent_server_header_above() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.entry_data_indices = vec![Some(0)];
        state.entry_group_keys = vec![None];
        state.picker_state.selected = 0;
        assert_eq!(state.selected_mcp_tool(), None);
    }

    // ── fuzzy_matches ────────────────────────────────────────────────

    #[test]
    fn fuzzy_matches_empty_query_matches_everything() {
        assert!(fuzzy_matches("anything", ""));
    }

    #[test]
    fn fuzzy_matches_substring() {
        assert!(fuzzy_matches("rust-check", "check"));
        assert!(fuzzy_matches("rust-check", "rust"));
        assert!(fuzzy_matches("Rust-Check", "check")); // case insensitive
    }

    #[test]
    fn fuzzy_matches_subsequence() {
        assert!(fuzzy_matches("rust-check", "rc")); // r...c
        assert!(fuzzy_matches("frontend-design", "fd")); // f...d
    }

    #[test]
    fn fuzzy_matches_rejects_non_matching() {
        assert!(!fuzzy_matches("hello", "xyz"));
        assert!(!fuzzy_matches("abc", "abdc")); // query longer than would match
    }

    // ── Skills search: substring-only, title-first ordering ─────────

    fn make_skill(
        name: &str,
        desc: &str,
    ) -> echo_agent_tools::implementations::skills::types::SkillInfo {
        echo_agent_tools::implementations::skills::types::SkillInfo {
            name: name.to_string(),
            display_name: None,
            description: desc.to_string(),
            when_to_use: None,
            short_description: None,
            author: None,
            argument_hint: None,
            license: None,
            compatibility: None,
            metadata: None,
            path: "test".to_string(),
            scope: echo_agent_tools::implementations::skills::types::SkillScope::User,
            config_source: None,
            plugin_name: None,
            plugin_version: None,
            plugin_root: None,
            plugin_data: None,
            allowed_tools: None,
            model: None,
            effort: None,
            user_invocable: false,
            disable_model_invocation: false,
            has_user_specified_description: false,
            paths: None,
            enabled: true,
            body: None,
        }
    }

    /// Skills search uses substring (not fuzzy), so "pdf" should NOT match
    /// "product design framework" even though p, d, f appear in order.
    #[test]
    fn skills_search_is_substring_not_fuzzy() {
        let skills = [
            make_skill("pdf", "PDF manipulation"),
            make_skill("product-design-framework", "Design framework for products"),
        ];
        let query = "pdf";
        let query_lower = query.to_lowercase();
        let matches: Vec<(usize, bool)> = skills
            .iter()
            .enumerate()
            .filter_map(|(si, skill)| {
                let name_lower = skill.name.to_lowercase();
                let desc_lower = skill.description.to_lowercase();
                let name_hit = name_lower.contains(&query_lower);
                let desc_hit = desc_lower.contains(&query_lower);
                if name_hit {
                    Some((si, true))
                } else if desc_hit {
                    Some((si, false))
                } else {
                    None
                }
            })
            .collect();

        // Only "pdf" should match, not "product-design-framework".
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], (0, true));
    }

    /// Title matches should sort before description-only matches.
    #[test]
    fn skills_search_title_matches_first() {
        let skills = [
            make_skill("some-tool", "Run lint check"), // desc match only
            make_skill("check", "Run lint check"),     // name match
            make_skill("rust-check", "Rust pre-push checks"), // name match
        ];
        let query = "check";
        let query_lower = query.to_lowercase();
        let mut matches: Vec<(usize, bool)> = skills
            .iter()
            .enumerate()
            .filter_map(|(si, skill)| {
                let name_lower = skill.name.to_lowercase();
                let desc_lower = skill.description.to_lowercase();
                let name_hit = name_lower.contains(&query_lower);
                let desc_hit = desc_lower.contains(&query_lower);
                if name_hit {
                    Some((si, true))
                } else if desc_hit {
                    Some((si, false))
                } else {
                    None
                }
            })
            .collect();
        matches.sort_by_key(|&(_, is_name)| !is_name);

        assert_eq!(matches.len(), 3);
        // Name matches first (check, rust-check), then desc-only (some-tool).
        assert!(matches[0].1, "first result should be a name match");
        assert!(matches[1].1, "second result should be a name match");
        assert!(!matches[2].1, "third result should be a desc-only match");
    }

    // ── Hooks: search forces groups expanded ─────────────────────────

    #[test]
    fn hooks_collapsed_groups_ignored_during_search() {
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert("global/hooks".to_string());

        // When query is empty, collapsed groups stay collapsed.
        let query = "";
        let is_collapsed_no_query = collapsed.contains("global/hooks") && query.is_empty();
        assert!(is_collapsed_no_query);

        // When query is non-empty, collapsed groups are forced open.
        let query = "safety";
        let is_collapsed_with_query = collapsed.contains("global/hooks") && query.is_empty();
        assert!(!is_collapsed_with_query);
    }

    // ── Skills: plugin skills appear in filter results ─────────────

    fn make_plugin_skill(
        name: &str,
        desc: &str,
        plugin: &str,
    ) -> echo_agent_tools::implementations::skills::types::SkillInfo {
        let mut skill = make_skill(name, desc);
        skill.plugin_name = Some(plugin.to_string());
        skill.scope = echo_agent_tools::implementations::skills::types::SkillScope::Plugin;
        skill.config_source = Some(
            echo_agent_tools::types::config_source::ConfigSource::Plugin {
                plugin_name: plugin.to_string(),
                path: std::path::PathBuf::from(format!("/plugins/{plugin}/skills/{name}/SKILL.md")),
            },
        );
        skill
    }

    #[test]
    fn plugin_skills_appear_in_filter_results() {
        let skills = vec![
            make_skill("rust-check", "Run Rust checks"),
            make_plugin_skill("hello", "A greeting skill", "example-plugin"),
            make_plugin_skill("lint", "Run lint checks", "linter"),
        ];
        let result = filter_and_sort_skills(&skills, "", StatusFilter::All);
        // All three skills (native + plugin) should appear.
        assert_eq!(result.matches.len(), 3);
    }

    #[test]
    fn plugin_skills_appear_with_enabled_filter() {
        let mut plugin_skill = make_plugin_skill("hello", "A greeting", "example-plugin");
        plugin_skill.enabled = true;
        let mut disabled_skill = make_plugin_skill("lint", "Lint checks", "linter");
        disabled_skill.enabled = false;
        let skills = vec![
            make_skill("native", "A native skill"),
            plugin_skill,
            disabled_skill,
        ];
        let result = filter_and_sort_skills(&skills, "", StatusFilter::Enabled);
        // Only enabled skills: native + hello (lint is disabled).
        assert_eq!(result.matches.len(), 2);
        // Verify the correct skills are returned: native (index 0) and hello (index 1).
        let indices: Vec<usize> = result.matches.iter().map(|m| m.skill_index).collect();
        assert!(indices.contains(&0), "native skill should be present");
        assert!(
            indices.contains(&1),
            "plugin skill 'hello' should be present"
        );
        assert!(
            !indices.contains(&2),
            "disabled plugin skill 'lint' should be excluded"
        );
    }

    #[test]
    fn plugin_skills_searchable_by_name() {
        let skills = vec![
            make_skill("rust-check", "Run Rust checks"),
            make_plugin_skill("hello", "A greeting skill", "example-plugin"),
        ];
        let result = filter_and_sort_skills(&skills, "hello", StatusFilter::All);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].skill_index, 1); // index of hello
    }

    #[test]
    fn plugin_skill_searchable_by_label_and_slash_identity() {
        let mut skill = make_plugin_skill("deploy-prod", "Ship it", "example-plugin");
        skill.display_name = Some("friendly".to_string());
        let skills = vec![skill];
        // "friendly" hits only the label; "prod" hits only the slash name.
        let by_label = filter_and_sort_skills(&skills, "friendly", StatusFilter::All);
        let by_name = filter_and_sort_skills(&skills, "prod", StatusFilter::All);
        assert_eq!(by_label.matches.len(), 1);
        assert_eq!(by_name.matches.len(), 1);
    }

    // ── Skills: selection clamping after filter ──────────────────────

    #[test]
    fn skills_selection_clamped_after_filter() {
        // User had selected index 10, but after filtering only 3 match.
        let mut selected: usize = 10;
        let match_count = 3usize;

        if match_count > 0 {
            selected = selected.min(match_count - 1);
        }
        assert_eq!(selected, 2); // clamped to last valid index
    }

    #[test]
    fn skills_selection_zero_when_one_match() {
        let mut selected: usize = 5;
        let match_count = 1usize;

        if match_count > 0 {
            selected = selected.min(match_count - 1);
        }
        assert_eq!(selected, 0);
    }

    #[test]
    fn workflows_tab_renders_catalog_flat() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Workflows);
        state.workflows_data = TabDataState::Loaded(vec![WorkflowInfo {
            name: "fix-ci".to_string(),
            description: "Fix failing CI on the current PR".to_string(),
            when_to_use: Some("when CI is red".to_string()),
            source: "builtin".to_string(),
            path: None,
        }]);
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);

        assert_eq!(
            buffer_count(&buf, "fix-ci"),
            1,
            "the workflow name must render as a flat row"
        );
        assert_eq!(
            buffer_count(&buf, "(builtin)"),
            1,
            "the workflow source must render as the right label"
        );
        assert!(
            !state
                .entry_labels_cache
                .iter()
                .any(|l| l.starts_with("Workflows (")),
            "the flat catalog must not render a group header"
        );
        assert!(
            state.entry_data_indices.iter().all(|d| d.is_none()),
            "workflow rows are browse-only"
        );
        assert!(
            state.entry_group_keys.iter().all(|k| k.is_none()),
            "workflow rows are not collapsible groups"
        );
    }

    #[test]
    fn workflows_tab_shows_all_entries_without_unusable_name_badge() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Workflows);
        state.workflows_data = TabDataState::Loaded(vec![
            WorkflowInfo {
                name: "valid-workflow".into(),
                description: "Valid".into(),
                when_to_use: None,
                source: "project".into(),
                path: None,
            },
            WorkflowInfo {
                name: "Not Launchable".into(),
                description: "Invalid".into(),
                when_to_use: None,
                source: "project".into(),
                path: None,
            },
        ]);
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert_eq!(buffer_count(&buf, "valid-workflow"), 1);
        assert_eq!(
            buffer_count(&buf, "Not Launchable"),
            1,
            "odd-named catalog entries still render"
        );
        assert_eq!(
            buffer_count(&buf, "[no slash command]"),
            0,
            "no unusable-name badge"
        );
    }

    #[test]
    fn workflows_tab_empty_shows_placeholder() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Workflows);
        state.workflows_data = TabDataState::Loaded(vec![]);
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert_eq!(
            state.entry_labels_cache,
            [workflows_picker_rows::WORKFLOWS_EMPTY_PLACEHOLDER]
        );
        assert_eq!(
            buffer_count(&buf, "No workflows available"),
            1,
            "empty catalog renders the dimmed placeholder row"
        );
    }

    #[test]
    fn workflows_tab_error_renders_dimmed_row() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Workflows);
        state.workflows_data = TabDataState::Error("boom".into());
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert_eq!(buffer_count(&buf, "Error: boom"), 1);
    }

    #[test]
    fn workflows_tab_loading_shows_spinner_not_placeholder() {
        // `ExtensionsModalState::new` starts `workflows_data` at Loading.
        let mut state = ExtensionsModalState::new(ExtensionsTab::Workflows);
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert!(state.entry_labels_cache.is_empty(), "no rows while loading");
        assert_eq!(
            buffer_count(&buf, "Loading"),
            1,
            "loading spinner shown instead of the empty placeholder"
        );
        assert_eq!(buffer_count(&buf, "No workflows available"), 0);
    }

    #[test]
    fn workflows_reload_key_resolves_reload_skills() {
        // resolve_key's match is non-exhaustive; pin the Workflows arm.
        assert!(matches!(
            resolve_key(ExtensionsTab::Workflows, 'r'),
            Some(ButtonAction::ReloadSkills)
        ));
    }

    // ── Plugin fixtures ─────────────────────────────────────────────

    fn make_plugin(name: &str) -> echo_agent_hooks_plugins_types::PluginInfo {
        test_plugin_info(name, None)
    }

    fn make_plugin_with_origin(
        name: &str,
        origin: echo_agent_hooks_plugins_types::PluginOrigin,
    ) -> echo_agent_hooks_plugins_types::PluginInfo {
        test_plugin_info(name, Some(origin))
    }

    // ── StatusFilter unit tests ─────────────────────────────────────

    #[test]
    fn status_filter_next_cycles() {
        assert_eq!(StatusFilter::All.next(), StatusFilter::Enabled);
        assert_eq!(StatusFilter::Enabled.next(), StatusFilter::Disabled);
        assert_eq!(StatusFilter::Disabled.next(), StatusFilter::All);
    }

    #[test]
    fn status_filter_matches() {
        assert!(StatusFilter::All.matches(true));
        assert!(StatusFilter::All.matches(false));
        assert!(StatusFilter::Enabled.matches(true));
        assert!(!StatusFilter::Enabled.matches(false));
        assert!(!StatusFilter::Disabled.matches(true));
        assert!(StatusFilter::Disabled.matches(false));
    }

    fn make_plugin_with_enabled(
        name: &str,
        enabled: bool,
    ) -> echo_agent_hooks_plugins_types::PluginInfo {
        let mut p = make_plugin(name);
        p.enabled = enabled;
        p
    }

    #[test]
    fn space_footer_desc_is_contextual_enable_or_disable() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Plugins);
        assert_eq!(
            action_key_footer_desc(' ', "toggle", &state),
            "enable/disable"
        );
        assert_eq!(action_key_cheatsheet_desc(' ', "toggle"), "enable/disable");

        state.entry_data_indices = vec![Some(0), Some(1)];
        state.picker_state.selected = 0;
        state.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse {
                plugins: vec![
                    make_plugin_with_enabled("on", true),
                    make_plugin_with_enabled("off", false),
                ],
            });

        assert_eq!(state.selected_item_enabled(), Some(true));
        assert_eq!(action_key_footer_desc(' ', "toggle", &state), "disable");

        state.picker_state.selected = 1;
        assert_eq!(state.selected_item_enabled(), Some(false));
        assert_eq!(action_key_footer_desc(' ', "toggle", &state), "enable");

        assert_eq!(action_key_footer_desc('a', "install", &state), "install");
        assert_eq!(action_key_footer_desc('r', "reload", &state), "reload");
        assert_eq!(action_key_cheatsheet_desc('a', "install"), "install");
    }

    /// Regression: footer Space verb must follow the *current*
    /// entry-mapping (post filter/query/tab), not a stale one from the previous
    /// list shape. Render passes freshly built locals into
    /// `action_key_footer_desc_for_mapping` (state publish is post-paint only).
    #[test]
    fn space_footer_follows_refreshed_entry_data_indices_after_filter_shape_change() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Plugins);
        state.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse {
                plugins: vec![
                    make_plugin_with_enabled("on", true),
                    make_plugin_with_enabled("off", false),
                ],
            });

        // Unfiltered shape: both rows; selection on the enabled plugin.
        let unfiltered = vec![Some(0), Some(1)];
        state.picker_state.selected = 0;
        assert_eq!(
            action_key_footer_desc_for_mapping(' ', "toggle", &state, &unfiltered, &[], 0,),
            "disable"
        );

        // Filtered to the disabled plugin only — same selected *row* index 0,
        // but it now maps to data index 1. Stale [Some(0), Some(1)] would still
        // report "disable"; the refreshed mapping must report "enable".
        let filtered = vec![Some(1)];
        assert_eq!(
            selected_item_enabled_at(&state, &filtered, &[], 0),
            Some(false)
        );
        assert_eq!(
            action_key_footer_desc_for_mapping(' ', "toggle", &state, &filtered, &[], 0),
            "enable"
        );

        // Published-state path (input handling / unit tests) still works when
        // state.entry_data_indices is set explicitly.
        state.entry_data_indices = filtered;
        assert_eq!(state.selected_item_enabled(), Some(false));
        assert_eq!(action_key_footer_desc(' ', "toggle", &state), "enable");
    }

    #[test]
    fn tab_all_hints_derives_from_action_keys_with_space_enable_disable() {
        for tab in [
            ExtensionsTab::Hooks,
            ExtensionsTab::Plugins,
            ExtensionsTab::Skills,
            ExtensionsTab::Workflows,
            ExtensionsTab::McpServers,
        ] {
            let keys = extensions_action_keys(tab);
            let hints = tab_all_hints(tab);
            for (ch, label) in keys {
                let expected = action_key_cheatsheet_desc(ch, label);
                assert!(
                    hints.iter().any(|h| h.label == expected),
                    "tab_all_hints({tab:?}) missing display label {expected:?} for key {ch:?}"
                );
            }
            assert!(
                !hints.iter().any(|h| h.label == "toggle"),
                "tab_all_hints({tab:?}) must not show raw toggle"
            );
        }
        let plugins = tab_all_hints(ExtensionsTab::Plugins);
        assert!(plugins.iter().any(|h| h.label == "install"));
        assert!(!plugins.iter().any(|h| h.label == "add"));
    }

    #[test]
    fn plugins_install_key_still_resolves_to_install_input() {
        match resolve_key(ExtensionsTab::Plugins, 'a') {
            Some(ButtonAction::StartInput { command_prefix, .. }) => {
                assert_eq!(command_prefix, "plugins_install");
            }
            other => panic!("expected plugins install StartInput, got {other:?}"),
        }
        assert_eq!(
            action_telemetry_label(ExtensionsTab::Plugins, 'a').as_deref(),
            Some("install")
        );
        assert_eq!(
            action_telemetry_label(ExtensionsTab::Plugins, ' ').as_deref(),
            Some("toggle")
        );
    }

    // ── Tab navigation ──────────────────────────────────────────────

    #[test]
    fn tab_next_wraps_around() {
        assert_eq!(ExtensionsTab::Hooks.next(), ExtensionsTab::Plugins);
        assert_eq!(ExtensionsTab::Plugins.next(), ExtensionsTab::Marketplace);
        assert_eq!(ExtensionsTab::Marketplace.next(), ExtensionsTab::Skills);
        assert_eq!(ExtensionsTab::Skills.next(), ExtensionsTab::Workflows);
        assert_eq!(ExtensionsTab::Workflows.next(), ExtensionsTab::McpServers);
        assert_eq!(ExtensionsTab::McpServers.next(), ExtensionsTab::Hooks);
    }

    #[test]
    fn tab_prev_wraps_around() {
        assert_eq!(ExtensionsTab::Hooks.prev(), ExtensionsTab::McpServers);
        assert_eq!(ExtensionsTab::McpServers.prev(), ExtensionsTab::Workflows);
        assert_eq!(ExtensionsTab::Workflows.prev(), ExtensionsTab::Skills);
        assert_eq!(ExtensionsTab::Skills.prev(), ExtensionsTab::Marketplace);
        assert_eq!(ExtensionsTab::Marketplace.prev(), ExtensionsTab::Plugins);
        assert_eq!(ExtensionsTab::Plugins.prev(), ExtensionsTab::Hooks);
    }

    #[test]
    fn tab_all_contains_six_tabs() {
        assert_eq!(ExtensionsTab::ALL.len(), 6);
    }

    // ── Modal state init ────────────────────────────────────────────

    #[test]
    fn modal_state_starts_loading() {
        let state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        assert_eq!(state.active_tab, ExtensionsTab::McpServers);
        assert!(matches!(state.mcps_data, TabDataState::Loading));
        assert!(matches!(state.skills_data, TabDataState::Loading));
        assert!(state.mcps_tools_expanded.is_empty());
        assert!(
            !state.mcps_collapsed_sections.contains("mcp-section:local"),
            "Local section starts expanded by default for a less noisy initial view"
        );
        assert!(!state.mcps_section_collapse_initialized);
        assert!(state.skills_expanded.is_empty());
        assert_eq!(state.skills_selected, 0);
    }

    // ── Bracketed paste ─────────────────────────────────────────────

    fn single_field_input(prefix: &str) -> ModalInput {
        ModalInput::from_specs(
            prefix.into(),
            vec![FieldSpec {
                label: "URL".into(),
                required: true,
                placeholder: None,
            }],
        )
    }

    fn mcp_add_input() -> ModalInput {
        // Field order matches `extensions_action_keys`: URL first, Name second.
        ModalInput::from_specs(
            "mcp_add".into(),
            vec![
                FieldSpec {
                    label: "URL / Command".into(),
                    required: true,
                    placeholder: None,
                },
                FieldSpec {
                    label: "Name".into(),
                    required: false,
                    placeholder: None,
                },
            ],
        )
    }

    #[test]
    fn apply_paste_inserts_url_into_focused_field_and_strips_newline() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.input = Some(single_field_input("test"));
        assert!(state.apply_paste("https://mcp.linear.app/mcp\n"));
        let field = state.input.as_ref().unwrap().field(0).unwrap();
        assert_eq!(field.text(), "https://mcp.linear.app/mcp");
        assert_eq!(field.cursor_byte(), "https://mcp.linear.app/mcp".len());
    }

    #[test]
    fn apply_paste_inserts_at_cursor_position() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text("AB");
        let _ = input.field_mut(0).unwrap().set_cursor_byte(1);
        state.input = Some(input);
        assert!(state.apply_paste("XY"));
        let field = state.input.as_ref().unwrap().field(0).unwrap();
        assert_eq!(field.text(), "AXYB");
        assert_eq!(field.cursor_byte(), 3);
    }

    #[test]
    fn apply_paste_strips_crlf() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.input = Some(single_field_input("test"));
        state.input.as_mut().unwrap().error = Some("Required: URL".to_owned());
        assert!(state.apply_paste("foo\r\nbar"));
        assert_eq!(
            state.input.as_ref().unwrap().field(0).unwrap().text(),
            "foobar"
        );
        assert!(state.input.as_ref().unwrap().error.is_none());
    }

    #[test]
    fn apply_paste_empty_is_noop() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.input = Some(single_field_input("test"));
        assert!(!state.apply_paste("\n\r"));
        let field = state.input.as_ref().unwrap().field(0).unwrap();
        assert_eq!(field.text(), "");
        assert_eq!(field.cursor_byte(), 0);
    }

    #[test]
    fn apply_paste_routes_to_search_when_no_input() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Plugins);
        state.picker_state.search_active = true;
        assert!(state.apply_paste("query"));
        assert_eq!(state.picker_state.query(), "query");
    }

    #[test]
    fn apply_paste_ignored_when_idle() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Plugins);
        assert!(!state.apply_paste("hello"));
        assert_eq!(state.picker_state.query(), "");
        assert!(state.input.is_none());
    }

    #[test]
    fn apply_paste_prefers_input_over_search() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.input = Some(single_field_input("test"));
        state.picker_state.search_active = true;
        assert!(state.apply_paste("url"));
        assert_eq!(
            state.input.as_ref().unwrap().field(0).unwrap().text(),
            "url"
        );
        assert_eq!(state.picker_state.query(), "");
    }

    #[test]
    fn apply_paste_targets_focused_field_in_multi_field() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        let mut input = mcp_add_input();
        let _ = input.handle_key(&key_event(KeyCode::Tab, KeyModifiers::NONE));
        state.input = Some(input);
        assert!(state.apply_paste("my-server"));
        let input = state.input.as_ref().unwrap();
        assert_eq!(input.field(0).unwrap().text(), "");
        assert_eq!(input.field(1).unwrap().text(), "my-server");
    }

    #[test]
    fn multi_field_form_field_texts() {
        let mut input = mcp_add_input();
        input.field_mut(0).unwrap().set_text("https://example.com");
        input.field_mut(1).unwrap().set_text("my-server");
        let texts = input.field_texts();
        assert_eq!(texts, vec!["https://example.com", "my-server"]);
    }

    #[test]
    fn from_specs_creates_empty_fields() {
        let input = mcp_add_input();
        assert_eq!(input.fields().len(), 2);
        assert!(input.field(0).unwrap().text().is_empty());
        assert!(input.field(1).unwrap().text().is_empty());
        assert!(input.field(0).unwrap().required());
        assert!(!input.field(1).unwrap().required());
        assert_eq!(input.focused_index(), 0);
    }

    // ── build_action_from_input / parse_mcp_add_fields ──────────────

    // Field order in submission: [URL / Command, Name]. URL is required.

    #[test]
    fn mcp_add_url_derives_name() {
        let texts = vec!["https://mcp.linear.app/mcp".into(), "".into()];
        let action = build_action_from_input("mcp_add", &texts);
        match action {
            Some(ButtonAction::AddMcpServer { name, config }) => {
                assert_eq!(name, "linear");
                assert!(matches!(
                    config.transport,
                    echo_agent_runtime::util::config::McpServerTransportConfig::StreamableHttp { .. }
                ));
            }
            other => panic!("expected AddMcpServer, got {other:?}"),
        }
    }

    #[test]
    fn mcp_add_explicit_name_and_url() {
        let texts = vec!["https://example.com".into(), "my-server".into()];
        let action = build_action_from_input("mcp_add", &texts);
        match action {
            Some(ButtonAction::AddMcpServer { name, .. }) => {
                assert_eq!(name, "my-server");
            }
            other => panic!("expected AddMcpServer, got {other:?}"),
        }
    }

    #[test]
    fn mcp_add_command_with_args() {
        let texts = vec!["npx -y @some/mcp".into(), "srv".into()];
        let action = build_action_from_input("mcp_add", &texts);
        match action {
            Some(ButtonAction::AddMcpServer { name, config }) => {
                assert_eq!(name, "srv");
                match config.transport {
                    echo_agent_runtime::util::config::McpServerTransportConfig::Stdio {
                        command,
                        args,
                        ..
                    } => {
                        assert_eq!(command, "npx");
                        assert_eq!(args, vec!["-y", "@some/mcp"]);
                    }
                    other => panic!("expected Stdio, got {other:?}"),
                }
            }
            other => panic!("expected AddMcpServer, got {other:?}"),
        }
    }

    #[test]
    fn mcp_add_empty_url_returns_none() {
        let texts = vec!["".into(), "name".into()];
        assert!(build_action_from_input("mcp_add", &texts).is_none());
    }

    #[test]
    fn build_action_single_field_plugins() {
        let texts = vec!["/path/to/plugin".into()];
        let action = build_action_from_input("plugins_install", &texts);
        assert!(matches!(
            action,
            Some(ButtonAction::PluginsAction(
                echo_agent_hooks_plugins_types::PluginsAction::Install { .. }
            ))
        ));
    }

    #[test]
    fn build_action_unknown_prefix_returns_none() {
        let texts = vec!["foo".into()];
        assert!(build_action_from_input("unknown", &texts).is_none());
    }

    // ── Key dispatch (ModalInput::handle_key) ───────────────────────

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn handle_key_esc_cancels() {
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text("some text");
        assert!(matches!(
            input.handle_key(&key_event(KeyCode::Esc, KeyModifiers::NONE)),
            ModalInputOutcome::Cancel
        ));
    }

    #[test]
    fn handle_key_char_inserts() {
        let mut input = single_field_input("test");
        input.handle_key(&key_event(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(input.field(0).unwrap().text(), "a");
        assert_eq!(input.field(0).unwrap().cursor_byte(), 1);
    }

    #[test]
    fn handle_key_ctrl_char_does_not_insert() {
        let mut input = single_field_input("test");
        let result = input.handle_key(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(matches!(result, ModalInputOutcome::Unchanged));
        assert!(input.field(0).unwrap().text().is_empty());
    }

    #[test]
    fn handle_key_backspace_deletes() {
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text("ab");
        input.handle_key(&key_event(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.field(0).unwrap().text(), "a");
        assert_eq!(input.field(0).unwrap().cursor_byte(), 1);
    }

    #[test]
    fn handle_key_delete_forward() {
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text("ab");
        let _ = input.field_mut(0).unwrap().set_cursor_byte(0);
        input.handle_key(&key_event(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(input.field(0).unwrap().text(), "b");
        assert_eq!(input.field(0).unwrap().cursor_byte(), 0);
    }

    #[test]
    fn handle_key_ctrl_u_kills_to_start() {
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text("hello world");
        let _ = input.field_mut(0).unwrap().set_cursor_byte(5);
        input.handle_key(&key_event(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(input.field(0).unwrap().text(), " world");
        assert_eq!(input.field(0).unwrap().cursor_byte(), 0);
    }

    #[test]
    fn handle_key_ctrl_k_kills_to_end() {
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text("hello world");
        let _ = input.field_mut(0).unwrap().set_cursor_byte(5);
        input.handle_key(&key_event(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(input.field(0).unwrap().text(), "hello");
        assert_eq!(input.field(0).unwrap().cursor_byte(), 5);
    }

    #[test]
    fn handle_key_tab_navigates_multi_field() {
        let mut input = mcp_add_input();
        assert_eq!(input.focused_index(), 0);
        input.handle_key(&key_event(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(input.focused_index(), 1);
        input.handle_key(&key_event(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(input.focused_index(), 0);
        input.handle_key(&key_event(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(input.focused_index(), 1);
    }

    #[test]
    fn modified_tab_chords_do_not_navigate_multi_field() {
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
        ] {
            let mut input = mcp_add_input();
            let outcome = input.handle_key(&key_event(KeyCode::Tab, modifiers));
            assert!(matches!(outcome, ModalInputOutcome::Unchanged));
            assert_eq!(input.focused_index(), 0);
        }
    }

    #[test]
    fn handle_key_submit_validates_required() {
        let mut input = mcp_add_input();
        let result = input.handle_key(&key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(result, ModalInputOutcome::Changed));
        assert!(input.error.is_some());
        assert!(input.error.as_ref().unwrap().contains("URL / Command"));
    }

    #[test]
    fn handle_key_submit_succeeds() {
        let mut input = mcp_add_input();
        input.field_mut(0).unwrap().set_text("https://example.com");
        let result = input.handle_key(&key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(result, ModalInputOutcome::Submit { .. }));
    }

    #[test]
    fn handle_key_home_end() {
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text("hello");
        let _ = input.field_mut(0).unwrap().set_cursor_byte(3);
        input.handle_key(&key_event(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(input.field(0).unwrap().cursor_byte(), 0);
        input.handle_key(&key_event(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(input.field(0).unwrap().cursor_byte(), 5);
    }

    #[test]
    fn handle_key_tab_completes_single_field_path() {
        let directory = tempfile::tempdir().unwrap();
        let completed = directory.path().join("plugin-source");
        std::fs::write(&completed, "").unwrap();
        let partial = directory.path().join("plugin-s");

        let mut input = single_field_input("test");
        input
            .field_mut(0)
            .unwrap()
            .set_text(partial.to_string_lossy());
        let outcome = input.handle_key(&key_event(KeyCode::Tab, KeyModifiers::NONE));

        assert!(matches!(outcome, ModalInputOutcome::Changed));
        let field = input.field(0).unwrap();
        assert_eq!(field.text(), completed.to_string_lossy().as_ref());
        assert_eq!(field.cursor_byte(), field.text().len());
    }

    #[test]
    fn modified_tab_chords_do_not_complete_single_field_path() {
        let directory = tempfile::tempdir().unwrap();
        let completed = directory.path().join("plugin-source");
        std::fs::write(&completed, "").unwrap();
        let partial = directory
            .path()
            .join("plugin-s")
            .to_string_lossy()
            .into_owned();

        for key in [
            key_event(KeyCode::Tab, KeyModifiers::SHIFT),
            key_event(KeyCode::BackTab, KeyModifiers::NONE),
            key_event(KeyCode::Tab, KeyModifiers::CONTROL),
            key_event(KeyCode::Tab, KeyModifiers::ALT),
            key_event(KeyCode::Tab, KeyModifiers::SUPER),
        ] {
            let mut input = single_field_input("test");
            input.field_mut(0).unwrap().set_text(&partial);
            let outcome = input.handle_key(&key);
            assert!(matches!(outcome, ModalInputOutcome::Unchanged));
            assert_eq!(input.field(0).unwrap().text(), partial);
            assert_eq!(input.focused_index(), 0);
        }
    }

    #[test]
    fn canonical_paste_shortcuts_include_super_and_exclude_altgr() {
        crate::clipboard::set_clipboard_probe_hook(
            crate::clipboard::ClipboardProbeHook::no_raster(Some("foo\r\nbar")),
        );
        let mut super_paste = single_field_input("test");
        let outcome = super_paste.handle_key(&key_event(KeyCode::Char('v'), KeyModifiers::SUPER));
        crate::clipboard::clear_clipboard_probe_hook();
        assert!(matches!(outcome, ModalInputOutcome::Changed));
        assert_eq!(super_paste.field(0).unwrap().text(), "foobar");

        crate::clipboard::set_clipboard_probe_hook(
            crate::clipboard::ClipboardProbeHook::no_raster(Some("clipboard")),
        );
        let mut altgr = single_field_input("test");
        let _ = altgr.handle_key(&key_event(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        crate::clipboard::clear_clipboard_probe_hook();
        assert_eq!(
            altgr.field(0).unwrap().text(),
            if cfg!(target_os = "windows") { "v" } else { "" }
        );
    }

    #[test]
    fn canonical_small_word_delete_differs_from_ctrl_w() {
        const URL: &str = "https://mcp.linear.app/mcp";
        for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            let mut input = single_field_input("test");
            input.field_mut(0).unwrap().set_text(URL);
            let outcome = input.handle_key(&key_event(KeyCode::Backspace, modifiers));
            assert!(matches!(outcome, ModalInputOutcome::Changed));
            assert_eq!(input.field(0).unwrap().text(), "https://mcp.linear.app/");
        }

        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text(URL);
        let outcome = input.handle_key(&key_event(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert!(matches!(outcome, ModalInputOutcome::Changed));
        assert_eq!(input.field(0).unwrap().text(), "");
    }

    #[test]
    fn alt_word_arrows_and_readline_bindings_are_equivalent() {
        for key in [
            key_event(KeyCode::Left, KeyModifiers::ALT),
            key_event(KeyCode::Char('b'), KeyModifiers::ALT),
            key_event(KeyCode::Left, KeyModifiers::CONTROL),
        ] {
            let mut input = single_field_input("test");
            input.field_mut(0).unwrap().set_text("hello-world");
            assert!(matches!(input.handle_key(&key), ModalInputOutcome::Changed));
            assert_eq!(input.field(0).unwrap().cursor_byte(), "hello-".len());
        }

        for key in [
            key_event(KeyCode::Right, KeyModifiers::ALT),
            key_event(KeyCode::Char('f'), KeyModifiers::ALT),
            key_event(KeyCode::Right, KeyModifiers::CONTROL),
        ] {
            let mut input = single_field_input("test");
            input.field_mut(0).unwrap().set_text("hello-world");
            let _ = input.field_mut(0).unwrap().set_cursor_byte(0);
            assert!(matches!(input.handle_key(&key), ModalInputOutcome::Changed));
            assert_eq!(input.field(0).unwrap().cursor_byte(), "hello".len());
        }
    }

    #[test]
    fn grapheme_delete_and_middle_insert_are_atomic() {
        let grapheme = "👩🏽\u{200d}💻";
        let mut input = single_field_input("test");
        input
            .field_mut(0)
            .unwrap()
            .set_text(format!("a{grapheme}b"));
        let _ = input.field_mut(0).unwrap().set_cursor_byte(1);

        assert!(matches!(
            input.handle_key(&key_event(KeyCode::Delete, KeyModifiers::NONE)),
            ModalInputOutcome::Changed
        ));
        assert_eq!(input.field(0).unwrap().text(), "ab");
        assert_eq!(input.field(0).unwrap().cursor_byte(), 1);

        assert!(matches!(
            input.handle_key(&key_event(KeyCode::Char('X'), KeyModifiers::NONE)),
            ModalInputOutcome::Changed
        ));
        assert_eq!(input.field(0).unwrap().text(), "aXb");
        assert_eq!(input.field(0).unwrap().cursor_byte(), 2);
    }

    #[test]
    fn cursor_and_handled_noop_edits_redraw_without_validation_changes() {
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text("abc");
        input.error = Some("Required: URL".to_owned());

        let outcome = input.handle_key(&key_event(KeyCode::Left, KeyModifiers::NONE));
        assert!(matches!(outcome, ModalInputOutcome::Changed));
        assert_eq!(input.field(0).unwrap().text(), "abc");
        assert_eq!(input.field(0).unwrap().cursor_byte(), 2);
        assert_eq!(input.focused_index(), 0);
        assert_eq!(input.error.as_deref(), Some("Required: URL"));

        let _ = input.handle_key(&key_event(KeyCode::Home, KeyModifiers::NONE));
        let outcome = input.handle_key(&key_event(KeyCode::Left, KeyModifiers::NONE));
        assert!(matches!(outcome, ModalInputOutcome::Changed));
        assert_eq!(input.field(0).unwrap().cursor_byte(), 0);
        assert_eq!(input.error.as_deref(), Some("Required: URL"));

        let outcome = input.handle_key(&key_event(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(matches!(outcome, ModalInputOutcome::Changed));
        assert_eq!(input.field(0).unwrap().text(), "xabc");
        assert!(input.error.is_none());
    }

    #[test]
    fn narrow_form_viewport_keeps_unicode_and_cursor_visible() {
        let grapheme = "👩🏽\u{200d}💻";
        let text = format!("1234567中e\u{301}{grapheme}b");
        let mut input = single_field_input("test");
        input.field_mut(0).unwrap().set_text(&text);
        let _ = input.field_mut(0).unwrap().set_cursor_byte(text.len() - 1);

        let area = Rect::new(0, 0, 20, 4);
        let theme = Theme::current();
        let mut buffer = Buffer::empty(area);
        let prompt_width = crate::glyphs::prompt_arrow().width();
        let editor_width = (area.width as usize - 8 - prompt_width).max(1);
        let viewport = input.field(0).unwrap().viewport(editor_width);
        let visible = &input.field(0).unwrap().text()[viewport.visible_byte_range.clone()];
        assert!(visible.contains('中'));
        assert!(visible.contains("e\u{301}"));
        assert!(visible.contains(grapheme));

        render_input_form(&mut buffer, area, &input, &theme);
        let rendered = (0..area.width).fold(String::new(), |mut line, x| {
            line.push_str(buffer[(x, 2)].symbol());
            line
        });
        assert!(rendered.contains('中'));
        assert!(rendered.contains("e\u{301}"));
        assert!(rendered.contains(grapheme));
        let text_x = 4 + prompt_width as u16;
        let cursor_x = text_x + viewport.cursor_display_column as u16;
        assert_eq!(buffer[(cursor_x, 2)].bg, theme.text_primary);
    }

    // ── Hook helpers with StatusFilter ───────────────────────────────

    fn make_hook(
        name: &str,
        source_dir: &str,
        disabled: bool,
    ) -> echo_agent_hooks_plugins_types::HookInfo {
        echo_agent_hooks_plugins_types::HookInfo {
            name: name.to_string(),
            event: echo_agent_hooks_plugins_types::HookEvent::PreToolUse,
            handler_type: echo_agent_hooks_plugins_types::HookHandlerType::Command,
            matcher: None,
            command: Some("/bin/true".to_string()),
            url: None,
            timeout_ms: 10_000,
            source_dir: source_dir.to_string(),
            disabled,
        }
    }

    #[test]
    fn build_hook_groups_respects_filter() {
        let hooks = vec![
            make_hook("a", "/src", false),   // enabled
            make_hook("b", "/src", true),    // disabled
            make_hook("c", "/other", false), // enabled
        ];
        let groups = build_hook_groups(&hooks, StatusFilter::Enabled, "");
        // Two custom groups, ordered A–Z by display label: /other before /src.
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].source_dir, "/other");
        assert_eq!(groups[0].indices, vec![2]);
        assert_eq!(groups[1].source_dir, "/src");
        assert_eq!(groups[1].indices, vec![0]);

        let groups_disabled = build_hook_groups(&hooks, StatusFilter::Disabled, "");
        // One group: /src with [1].
        assert_eq!(groups_disabled.len(), 1);
        assert_eq!(groups_disabled[0].indices, vec![1]);
    }

    #[test]
    fn modal_state_filters_default_to_all() {
        let state = ExtensionsModalState::new(ExtensionsTab::Hooks);
        assert_eq!(state.hooks_filter, StatusFilter::All);
        assert_eq!(state.plugins_filter, StatusFilter::All);
        assert_eq!(state.mcps_filter, StatusFilter::All);
    }

    // ── Marketplace tests (obra/superpowers as sample) ──────────────

    /// Build a realistic marketplace source modelled on obra/superpowers.
    fn superpowers_source() -> echo_agent_hooks_plugins_types::MarketplaceScanResult {
        echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: "superpowers".into(),
            source_kind: "git".into(),
            source_url_or_path: "https://github.com/obra/superpowers".into(),
            plugins: vec![
                TestPlugin {
                    name: "superpowers",
                    description: Some(
                        "An agentic skills framework & software development methodology",
                    ),
                    skill_count: 14,
                    has_hooks: true,
                    has_agents: true,
                    ..Default::default()
                }
                .build(),
                TestPlugin {
                    name: "brainstorming",
                    description: Some("Socratic design refinement before writing code"),
                    install_status: "installed",
                    ..Default::default()
                }
                .build(),
                TestPlugin {
                    name: "test-driven-development",
                    description: Some(
                        "RED-GREEN-REFACTOR cycle with testing anti-patterns reference",
                    ),
                    ..Default::default()
                }
                .build(),
                TestPlugin {
                    name: "systematic-debugging",
                    description: Some("4-phase root cause analysis process"),
                    install_status: "update_available",
                    ..Default::default()
                }
                .build(),
                TestPlugin {
                    name: "subagent-driven-development",
                    description: Some("Fast iteration with two-stage review"),
                    has_agents: true,
                    ..Default::default()
                }
                .build(),
            ],
            error: None,
        }
    }

    /// Test helper: marketplace plugin descriptor with sensible defaults.
    struct TestPlugin {
        name: &'static str,
        version: Option<&'static str>,
        description: Option<&'static str>,
        author: Option<&'static str>,
        skill_count: usize,
        has_hooks: bool,
        has_agents: bool,
        has_mcp: bool,
        install_status: &'static str,
        components: Option<echo_agent_hooks_plugins_types::PluginComponents>,
    }

    impl Default for TestPlugin {
        fn default() -> Self {
            Self {
                name: "",
                version: Some("5.1.0"),
                description: None,
                author: Some("obra"),
                skill_count: 1,
                has_hooks: false,
                has_agents: false,
                has_mcp: false,
                install_status: "not_installed",
                components: None,
            }
        }
    }

    impl TestPlugin {
        fn build(self) -> echo_agent_hooks_plugins_types::MarketplacePluginEntry {
            let installed_version = if self.install_status == "installed"
                || self.install_status == "update_available"
            {
                self.version.map(String::from)
            } else {
                None
            };
            echo_agent_hooks_plugins_types::MarketplacePluginEntry {
                name: self.name.to_string(),
                version: self.version.map(String::from),
                description: self.description.map(String::from),
                category: None,
                author: self.author.map(String::from),
                tags: vec![],
                keywords: vec![],
                domains: vec![],
                homepage: None,
                relative_path: format!("plugins/{}", self.name),
                skill_count: self.skill_count,
                has_hooks: self.has_hooks,
                has_agents: self.has_agents,
                has_mcp: self.has_mcp,
                install_status: self.install_status.to_string(),
                installed_version,
                components: self.components,
                remote_url: Some("https://github.com/obra/superpowers".into()),
                remote_ref: Some("main".into()),
                remote_sha: None,
                remote_subdir: None,
            }
        }
    }

    // ── Marketplace: resolve_marketplace_selection ───────────────────

    #[test]
    fn resolve_selection_on_source_header() {
        let sources = vec![superpowers_source()];
        let mut state = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        state.entry_data_indices = vec![None, Some(0), Some(0), Some(0), Some(0), Some(0)];
        state.entry_group_keys = vec![Some("0".into()), None, None, None, None, None];
        state.entry_labels_cache = vec![
            "superpowers (5 plugins)".into(),
            "superpowers".into(),
            "brainstorming".into(),
            "test-driven-development".into(),
            "systematic-debugging".into(),
            "subagent-driven-development".into(),
        ];
        state.picker_state.selected = 0; // source header
        let result = state.resolve_marketplace_selection(&sources);
        assert_eq!(result, Some((0, None)));
    }

    #[test]
    fn resolve_selection_on_plugin_entry() {
        let sources = vec![superpowers_source()];
        let mut state = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        state.entry_data_indices = vec![None, Some(0), Some(0), Some(0), Some(0), Some(0)];
        state.entry_group_keys = vec![Some("0".into()), None, None, None, None, None];
        state.entry_labels_cache = vec![
            "superpowers (5 plugins)".into(),
            "superpowers".into(),
            "brainstorming".into(),
            "test-driven-development".into(),
            "systematic-debugging".into(),
            "subagent-driven-development".into(),
        ];
        // Select "brainstorming" (picker index 2).
        state.picker_state.selected = 2;
        let result = state.resolve_marketplace_selection(&sources);
        assert_eq!(result, Some((0, Some(1)))); // source 0, plugin index 1

        // Select "systematic-debugging" (picker index 4).
        state.picker_state.selected = 4;
        let result = state.resolve_marketplace_selection(&sources);
        assert_eq!(result, Some((0, Some(3)))); // source 0, plugin index 3
    }

    // ── Marketplace: build_action_from_input ─────────────────────────

    #[test]
    fn marketplace_add_source_builds_action() {
        let texts = vec!["https://github.com/obra/superpowers".into()];
        let action = build_action_from_input("marketplace_add_source", &texts);
        match action {
            Some(ButtonAction::MarketplaceAction(
                echo_agent_hooks_plugins_types::MarketplaceAction::AddSource { url },
            )) => {
                assert_eq!(url, "https://github.com/obra/superpowers");
            }
            other => panic!("expected MarketplaceAction::AddSource, got {other:?}"),
        }
    }

    #[test]
    fn marketplace_add_source_trims_whitespace() {
        let texts = vec!["  https://github.com/obra/superpowers  ".into()];
        let action = build_action_from_input("marketplace_add_source", &texts);
        match action {
            Some(ButtonAction::MarketplaceAction(
                echo_agent_hooks_plugins_types::MarketplaceAction::AddSource { url },
            )) => {
                assert_eq!(url, "https://github.com/obra/superpowers");
            }
            other => panic!("expected MarketplaceAction::AddSource, got {other:?}"),
        }
    }

    // ── Marketplace: resolve_key dispatch ────────────────────────────

    #[test]
    fn marketplace_key_i_dispatches_install() {
        let action = resolve_key(ExtensionsTab::Marketplace, 'i');
        assert!(matches!(
            action,
            Some(ButtonAction::InstallSelectedMarketplacePlugin)
        ));
    }

    #[test]
    fn marketplace_key_d_dispatches_uninstall() {
        let action = resolve_key(ExtensionsTab::Marketplace, 'd');
        assert!(matches!(
            action,
            Some(ButtonAction::UninstallSelectedMarketplacePlugin)
        ));
    }

    #[test]
    fn marketplace_key_r_dispatches_refresh() {
        let action = resolve_key(ExtensionsTab::Marketplace, 'r');
        assert!(matches!(
            action,
            Some(ButtonAction::MarketplaceAction(
                echo_agent_hooks_plugins_types::MarketplaceAction::Refresh {
                    source_url_or_path: None
                }
            ))
        ));
    }

    #[test]
    fn marketplace_key_a_dispatches_add_source_input() {
        let action = resolve_key(ExtensionsTab::Marketplace, 'a');
        match action {
            Some(ButtonAction::StartInput {
                command_prefix,
                fields,
            }) => {
                assert_eq!(command_prefix, "marketplace_add_source");
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].label, "Source");
                assert!(fields[0].required);
            }
            other => panic!("expected StartInput for marketplace_add_source, got {other:?}"),
        }
    }

    #[test]
    fn marketplace_key_x_dispatches_remove_source() {
        let action = resolve_key(ExtensionsTab::Marketplace, 'x');
        assert!(matches!(
            action,
            Some(ButtonAction::RemoveSelectedMarketplaceSource)
        ));
    }

    #[test]
    fn marketplace_key_u_dispatches_update() {
        let action = resolve_key(ExtensionsTab::Marketplace, 'u');
        assert!(matches!(
            action,
            Some(ButtonAction::UpdateSelectedMarketplacePlugin)
        ));
    }

    #[test]
    fn plugins_key_u_dispatches_update_selected_plugin() {
        let action = resolve_key(ExtensionsTab::Plugins, 'u');
        assert!(matches!(action, Some(ButtonAction::UpdateSelectedPlugin)));
    }

    #[test]
    fn plugins_action_keys_match_resolve_key() {
        for &(ch, label) in &extensions_action_keys(ExtensionsTab::Plugins) {
            let action = resolve_key(ExtensionsTab::Plugins, ch);
            assert!(
                action.is_some(),
                "Plugins action key ('{ch}', \"{label}\") has no resolve_key arm"
            );
        }
    }

    #[test]
    fn result_notice_tick_counts_down_then_expires() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Plugins);
        state.result_notice = Some(ActionResultNotice {
            message: "x: updated".into(),
            entry_index: Some(0),
            ticks_remaining: 2,
        });
        assert!(!state.tick_result_notice(), "2 -> 1, still showing");
        assert!(
            !state.tick_result_notice(),
            "1 -> 0, still showing this frame"
        );
        assert!(state.tick_result_notice(), "0 -> expired (redraw to erase)");
        assert!(state.result_notice.is_none(), "cleared after expiry");
        assert!(!state.tick_result_notice(), "no notice -> no redraw");
    }

    #[test]
    fn marketplace_action_keys_match_resolve_key() {
        for &(ch, label) in &extensions_action_keys(ExtensionsTab::Marketplace) {
            let action = resolve_key(ExtensionsTab::Marketplace, ch);
            assert!(
                action.is_some(),
                "Marketplace action key ('{ch}', \"{label}\") has no resolve_key arm"
            );
        }
    }

    // ── Marketplace: fuzzy search over obra/superpowers plugins ──────

    #[test]
    fn marketplace_fuzzy_matches_superpowers_plugins() {
        let source = superpowers_source();
        // "tdd" fuzzy-matches "test-driven-development" (t...d...d).
        assert!(source.plugins.iter().any(|p| fuzzy_matches(&p.name, "tdd")));
        // "brain" substring-matches "brainstorming".
        assert!(
            source
                .plugins
                .iter()
                .any(|p| fuzzy_matches(&p.name, "brain"))
        );
        // "xyz" matches nothing.
        assert!(!source.plugins.iter().any(|p| fuzzy_matches(&p.name, "xyz")));
    }

    #[test]
    fn marketplace_fuzzy_matches_across_multiple_plugins() {
        let source = superpowers_source();
        // "sub" matches "subagent-driven-development" and "superpowers"
        // (s...u...b in both).
        let matches: Vec<&str> = source
            .plugins
            .iter()
            .filter(|p| fuzzy_matches(&p.name, "sub"))
            .map(|p| p.name.as_str())
            .collect();
        assert!(matches.contains(&"subagent-driven-development"));
    }

    // ── Marketplace: modal state with loaded marketplace data ────────

    #[test]
    fn marketplace_modal_state_with_loaded_data() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        state.marketplace_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::MarketplaceListResponse {
                sources: vec![superpowers_source()],
            });
        assert!(matches!(state.marketplace_data, TabDataState::Loaded(_)));
        assert_eq!(state.marketplace_selected, 0);
        assert_eq!(state.marketplace_scroll, 0);
    }

    #[test]
    fn marketplace_collapsed_sources_start_empty() {
        let state = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        assert!(state.marketplace_collapsed_sources.is_empty());
    }

    #[test]
    fn marketplace_is_group_expanded_default_open() {
        let state = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        // Source "0" is not in collapsed set, so it's expanded.
        assert!(state.is_group_expanded(0, "0"));
    }

    #[test]
    fn marketplace_is_group_expanded_collapsed_source() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        state.marketplace_collapsed_sources.insert(0);
        // Source "0" is now collapsed.
        assert!(!state.is_group_expanded(0, "0"));
    }

    #[test]
    fn marketplace_is_group_expanded_forced_open_during_search() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        state.marketplace_collapsed_sources.insert(0);
        state.picker_state.set_query("debug");
        // During search, collapsed sources are forced open.
        assert!(state.is_group_expanded(0, "0"));
    }

    // ── Marketplace: error source rendering ─────────────────────────

    #[test]
    fn marketplace_error_source_renders_header_with_error_badge() {
        let error_source = echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: "broken-source".into(),
            source_kind: "git".into(),
            source_url_or_path: "https://github.com/bad/repo".into(),
            plugins: vec![],
            error: Some("failed to clone: repository not found".into()),
        };
        let mut state = marketplace_modal_state(error_source);
        let buf = render_marketplace_into_buffer(&mut state, 100, 40);
        assert!(buffer_count(&buf, "broken-source") >= 1);
        assert!(buffer_count(&buf, "[error]") >= 1);
    }

    // ── Marketplace: components rendering + search ──────────────────

    fn component(name: &str, desc: Option<&str>) -> echo_agent_hooks_plugins_types::ComponentItem {
        echo_agent_hooks_plugins_types::ComponentItem::new(name, desc.map(str::to_string))
    }

    fn sample_components() -> echo_agent_hooks_plugins_types::PluginComponents {
        echo_agent_hooks_plugins_types::PluginComponents {
            skills: vec![
                component("brainstorming", Some("Structured ideation before coding")),
                component("test-driven-development", None),
            ],
            commands: vec![component("/brainstorm", Some("Start a brainstorm"))],
            hooks: vec![component("PreToolUse", Some("Bash"))],
            ..Default::default()
        }
    }

    #[test]
    fn marketplace_summary_uses_catalog_components_when_present() {
        let plugin = TestPlugin {
            name: "superpowers",
            components: Some(sample_components()),
            ..Default::default()
        }
        .build();
        assert_eq!(
            marketplace_components_summary(&plugin).as_deref(),
            Some("2 skills \u{b7} 1 command \u{b7} 1 hook")
        );
    }

    #[test]
    fn marketplace_summary_empty_components_is_none() {
        let plugin = TestPlugin {
            name: "empty",
            components: Some(echo_agent_hooks_plugins_types::PluginComponents::default()),
            ..Default::default()
        }
        .build();
        assert_eq!(marketplace_components_summary(&plugin), None);
    }

    #[test]
    fn marketplace_summary_ignores_legacy_fields() {
        let plugin = TestPlugin {
            name: "legacy",
            skill_count: 3,
            has_hooks: true,
            has_mcp: true,
            ..Default::default()
        }
        .build();
        assert_eq!(marketplace_components_summary(&plugin), None);
    }

    #[test]
    fn marketplace_summary_url_entry_without_data_is_none() {
        let plugin = TestPlugin {
            name: "remote",
            skill_count: 0,
            ..Default::default()
        }
        .build();
        assert!(plugin.remote_url.is_some());
        assert_eq!(marketplace_components_summary(&plugin), None);
    }

    #[test]
    fn marketplace_summary_local_entry_without_data_shows_nothing() {
        let mut plugin = TestPlugin {
            name: "bare",
            skill_count: 0,
            ..Default::default()
        }
        .build();
        plugin.remote_url = None;
        assert_eq!(marketplace_components_summary(&plugin), None);
    }

    #[test]
    fn render_components_fields_lists_names_only_per_category() {
        let fields = render_components_fields(&sample_components());
        assert_eq!(
            fields,
            vec![
                (
                    "skills".to_string(),
                    "brainstorming, test-driven-development".to_string()
                ),
                ("commands".to_string(), "/brainstorm".to_string()),
                ("hooks".to_string(), "PreToolUse".to_string()),
            ]
        );
    }

    #[test]
    fn render_components_fields_caps_names_per_category() {
        let components = echo_agent_hooks_plugins_types::PluginComponents {
            skills: (0..12)
                .map(|i| component(&format!("skill-{i}"), None))
                .collect(),
            ..Default::default()
        };
        let fields = render_components_fields(&components);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].0, "skills");
        assert_eq!(
            fields[0].1,
            "skill-0, skill-1, skill-2, skill-3, skill-4, skill-5, skill-6, skill-7 +4 more"
        );
    }

    #[test]
    fn render_components_fields_covers_all_six_categories() {
        let components = echo_agent_hooks_plugins_types::PluginComponents {
            skills: vec![component("s", None)],
            commands: vec![component("c", None)],
            agents: vec![component("a", None)],
            mcp_servers: vec![component("m", None)],
            hooks: vec![component("h", None)],
            lsp_servers: vec![component("l", None)],
        };
        let fields = render_components_fields(&components);
        let labels: Vec<&str> = fields.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "skills",
                "commands",
                "agents",
                "mcp servers",
                "hooks",
                "lsp servers"
            ]
        );
    }

    // ── Marketplace: collapsed/expanded row rendering ────────────────

    fn render_marketplace_into_buffer(state: &mut ExtensionsModalState, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, state, None, false, 0);
        buf
    }

    fn buffer_count(buf: &Buffer, needle: &str) -> usize {
        let area = *buf.area();
        let mut count = 0usize;
        for y in area.top()..area.bottom() {
            let mut row = String::new();
            for x in area.left()..area.right() {
                row.push_str(buf[(x, y)].symbol());
            }
            count += row.matches(needle).count();
        }
        count
    }

    fn marketplace_modal_state(
        source: echo_agent_hooks_plugins_types::MarketplaceScanResult,
    ) -> ExtensionsModalState {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        state.marketplace_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::MarketplaceListResponse {
                sources: vec![source],
            });
        state
    }

    #[test]
    fn marketplace_collapsed_row_shows_component_summary() {
        let mut source = superpowers_source();
        source.plugins[0].components = Some(sample_components());
        let mut state = marketplace_modal_state(source);
        let buf = render_marketplace_into_buffer(&mut state, 100, 40);
        assert!(
            state.picker_state.expanded.is_empty(),
            "rows must be collapsed by default for this test to pin anything"
        );
        assert_eq!(
            buffer_count(&buf, "2 skills \u{b7} 1 command \u{b7} 1 hook"),
            1,
            "collapsed marketplace row must show the catalog component summary"
        );
    }

    #[test]
    fn marketplace_legacy_fields_never_render() {
        let mut source = superpowers_source();
        source.plugins.truncate(1);
        let mut state = marketplace_modal_state(source);
        let legacy_needle = "14 skills";

        let collapsed_buf = render_marketplace_into_buffer(&mut state, 100, 40);
        assert_eq!(
            buffer_count(&collapsed_buf, legacy_needle),
            0,
            "legacy scan counts must not render on collapsed rows"
        );

        state.picker_state.expanded.insert(1);
        let expanded_buf = render_marketplace_into_buffer(&mut state, 100, 40);
        assert_eq!(
            buffer_count(&expanded_buf, legacy_needle),
            0,
            "legacy scan counts must not render in the expanded view"
        );
        assert_eq!(
            buffer_count(&expanded_buf, "contents shown after install"),
            1,
            "catalog-less URL entry shows the install hint in the provides field"
        );
    }

    #[test]
    fn marketplace_catalog_summary_not_duplicated_when_expanded() {
        let mut source = superpowers_source();
        source.plugins.truncate(1);
        source.plugins[0].components = Some(sample_components());
        let mut state = marketplace_modal_state(source);
        let summary = "2 skills \u{b7} 1 command \u{b7} 1 hook";

        let collapsed_buf = render_marketplace_into_buffer(&mut state, 100, 40);
        assert_eq!(
            buffer_count(&collapsed_buf, summary),
            1,
            "collapsed row shows the catalog summary once"
        );

        state.picker_state.expanded.insert(1);
        let expanded_buf = render_marketplace_into_buffer(&mut state, 100, 40);
        assert_eq!(
            buffer_count(&expanded_buf, summary),
            0,
            "expanded view replaces the summary with the enumeration"
        );
        assert_eq!(
            buffer_count(&expanded_buf, "brainstorming, test-driven-development"),
            1,
            "expanded view enumerates component names once"
        );
    }

    // ── Plugins: origin grouping ─────────────────────────────────────

    fn plugins_modal_state(
        plugins: Vec<echo_agent_hooks_plugins_types::PluginInfo>,
    ) -> ExtensionsModalState {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Plugins);
        state.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse { plugins });
        state
    }

    fn render_plugins_into_buffer(state: &mut ExtensionsModalState, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, state, None, false, 0);
        buf
    }

    #[test]
    fn plugin_group_maps_each_origin_variant() {
        use echo_agent_hooks_plugins_types::PluginOrigin;
        for (origin, rank, key, label) in [
            (
                PluginOrigin::ProjectEchoAgent,
                0,
                "origin:project",
                "Project",
            ),
            (
                PluginOrigin::ProjectClaude,
                1,
                "origin:project-claude",
                "Project (Claude)",
            ),
            (PluginOrigin::UserEchoAgent, 2, "origin:user", "User"),
            (
                PluginOrigin::UserClaude,
                3,
                "origin:user-claude",
                "User (Claude)",
            ),
            (
                PluginOrigin::ClaudeInstalled { marketplace: None },
                3,
                "origin:user-claude",
                "User (Claude)",
            ),
            (
                PluginOrigin::ClaudeMarketplace {
                    marketplace: "mp".into(),
                },
                4,
                "claude-mp:mp",
                "mp",
            ),
            (
                PluginOrigin::MarketplaceInstall {
                    source_name: Some("EchoAgent Official".into()),
                    git_url: Some("https://example.com/r.git".into()),
                },
                5,
                "echo-agent-mp:EchoAgent Official",
                "EchoAgent Official",
            ),
            (
                PluginOrigin::MarketplaceInstall {
                    source_name: None,
                    git_url: Some("https://example.com/r.git".into()),
                },
                6,
                "origin:direct",
                "Direct installs",
            ),
            (PluginOrigin::CliOverride, 7, "origin:cli", "CLI override"),
            (PluginOrigin::ConfigPath, 8, "origin:config", "Custom paths"),
        ] {
            let group = plugin_group(&make_plugin_with_origin("p", origin.clone()));
            assert_eq!(group.rank, rank, "{origin:?}");
            assert_eq!(group.key, key, "{origin:?}");
            assert_eq!(group.label, label, "{origin:?}");
        }
    }

    #[test]
    fn plugin_group_merges_claude_marketplace_and_installed() {
        use echo_agent_hooks_plugins_types::PluginOrigin;
        let catalog = plugin_group(&make_plugin_with_origin(
            "a",
            PluginOrigin::ClaudeMarketplace {
                marketplace: "mp".into(),
            },
        ));
        let installed = plugin_group(&make_plugin_with_origin(
            "b",
            PluginOrigin::ClaudeInstalled {
                marketplace: Some("mp".into()),
            },
        ));
        assert_eq!(catalog, installed);
    }

    #[test]
    fn plugin_group_fallback_without_origin() {
        let mut project = make_plugin("proj");
        project.scope = echo_agent_hooks_plugins_types::PluginScope::Project;
        assert_eq!(plugin_group(&project).key, "origin:project");

        let user = make_plugin("plain");
        assert_eq!(plugin_group(&user).key, "origin:user");

        let mut cli = make_plugin("cli-tool");
        cli.scope = echo_agent_hooks_plugins_types::PluginScope::Cli;
        assert_eq!(plugin_group(&cli).key, "origin:cli");

        let mut config = make_plugin("cfg-tool");
        config.scope = echo_agent_hooks_plugins_types::PluginScope::Config;
        assert_eq!(plugin_group(&config).key, "origin:config");

        let mut mp = make_plugin("mp-tool");
        mp.marketplace_source = Some("EchoAgent Official".into());
        let group = plugin_group(&mp);
        assert_eq!(group.key, "echo-agent-mp:EchoAgent Official");
        assert_eq!(group.label, "EchoAgent Official");

        let mut direct = make_plugin("direct-tool");
        direct.marketplace_source = Some("git: owner/repo".into());
        assert_eq!(plugin_group(&direct).key, "origin:direct");
    }

    #[test]
    fn plugin_group_unknown_origin_uses_scope_fallback() {
        let mut unknown = make_plugin_with_origin(
            "future-tool",
            echo_agent_hooks_plugins_types::PluginOrigin::Unknown,
        );
        assert_eq!(plugin_group(&unknown).key, "origin:user");

        unknown.marketplace_source = Some("EchoAgent Official".into());
        assert_eq!(
            plugin_group(&unknown).key,
            "echo-agent-mp:EchoAgent Official"
        );
    }

    #[test]
    fn plugins_render_groups_with_headers_in_rank_order() {
        use echo_agent_hooks_plugins_types::PluginOrigin;
        let mut state = plugins_modal_state(vec![
            make_plugin_with_origin(
                "mp-tool",
                PluginOrigin::ClaudeMarketplace {
                    marketplace: "claude-market".into(),
                },
            ),
            make_plugin_with_origin("user-tool", PluginOrigin::UserEchoAgent),
            make_plugin_with_origin("claude-tool", PluginOrigin::UserClaude),
        ]);
        let buf = render_plugins_into_buffer(&mut state, 100, 40);

        assert_eq!(buffer_count(&buf, "User (1 plugin)"), 1);
        assert_eq!(buffer_count(&buf, "User (Claude) (1 plugin)"), 1);
        assert_eq!(buffer_count(&buf, "claude-market (1 plugin)"), 1);
        assert_eq!(buffer_count(&buf, "user-tool"), 1);
        assert_eq!(buffer_count(&buf, "claude-tool"), 1);
        assert_eq!(buffer_count(&buf, "mp-tool"), 1);

        assert_eq!(
            state.entry_group_keys,
            vec![
                Some("origin:user".to_string()),
                None,
                Some("origin:user-claude".to_string()),
                None,
                Some("claude-mp:claude-market".to_string()),
                None,
            ]
        );
        assert_eq!(
            state.entry_data_indices,
            vec![None, Some(1), None, Some(2), None, Some(0)]
        );
    }

    #[test]
    fn plugins_render_multiple_plugins_under_one_group() {
        use echo_agent_hooks_plugins_types::PluginOrigin;
        let mut state = plugins_modal_state(vec![
            make_plugin_with_origin("solo-tool", PluginOrigin::UserEchoAgent),
            make_plugin_with_origin(
                "catalog-tool",
                PluginOrigin::ClaudeMarketplace {
                    marketplace: "claude-market".into(),
                },
            ),
            make_plugin_with_origin(
                "installed-tool",
                PluginOrigin::ClaudeInstalled {
                    marketplace: Some("claude-market".into()),
                },
            ),
        ]);
        let buf = render_plugins_into_buffer(&mut state, 100, 40);

        assert_eq!(
            buffer_count(&buf, "claude-market (2 plugins)"),
            1,
            "catalog and installed entries for the same marketplace share one group"
        );
        assert_eq!(buffer_count(&buf, "catalog-tool"), 1);
        assert_eq!(buffer_count(&buf, "installed-tool"), 1);

        assert_eq!(
            state.entry_group_keys,
            vec![
                Some("origin:user".to_string()),
                None,
                Some("claude-mp:claude-market".to_string()),
                None,
                None,
            ]
        );
        // Within the shared marketplace group, children are A–Z by name:
        // catalog-tool (1) before installed-tool (2).
        assert_eq!(
            state.entry_data_indices,
            vec![None, Some(0), None, Some(1), Some(2)],
            "children A–Z by name within their group (catalog before installed)"
        );

        state
            .plugins_collapsed_groups
            .insert("claude-mp:claude-market".into());
        let buf = render_plugins_into_buffer(&mut state, 100, 40);
        assert_eq!(buffer_count(&buf, "catalog-tool"), 0);
        assert_eq!(buffer_count(&buf, "installed-tool"), 0);
        assert_eq!(
            buffer_count(&buf, "solo-tool"),
            1,
            "collapsing one group must not hide siblings"
        );
    }

    #[test]
    fn plugins_collapsed_group_hides_rows_and_search_forces_open() {
        use echo_agent_hooks_plugins_types::PluginOrigin;
        let mut plugin = make_plugin_with_origin("user-tool", PluginOrigin::UserEchoAgent);
        plugin.root = "/opt/p1".into();
        let mut state = plugins_modal_state(vec![plugin]);
        state.plugins_collapsed_groups.insert("origin:user".into());

        let buf = render_plugins_into_buffer(&mut state, 100, 40);
        assert_eq!(buffer_count(&buf, "User (1 plugin)"), 1);
        assert_eq!(buffer_count(&buf, "user-tool"), 0);

        state.picker_state.set_query("user");
        let buf = render_plugins_into_buffer(&mut state, 100, 40);
        assert_eq!(
            buffer_count(&buf, "user-tool"),
            1,
            "search must flatten collapsed groups"
        );
    }

    #[test]
    fn plugins_fallback_grouping_without_origin() {
        let mut direct = make_plugin("direct-tool");
        direct.marketplace_source = Some("git: owner/repo".into());
        let mut mp = make_plugin("official-tool");
        mp.marketplace_source = Some("EchoAgent Official".into());
        let plain = make_plugin("plain-tool");

        let mut state = plugins_modal_state(vec![direct, mp, plain]);
        let buf = render_plugins_into_buffer(&mut state, 100, 40);

        assert_eq!(buffer_count(&buf, "User (1 plugin)"), 1);
        assert_eq!(buffer_count(&buf, "EchoAgent Official (1 plugin)"), 1);
        assert_eq!(buffer_count(&buf, "Direct installs (1 plugin)"), 1);
    }

    #[test]
    fn plugins_status_filter_omits_empty_groups() {
        use echo_agent_hooks_plugins_types::PluginOrigin;
        let mut disabled = make_plugin_with_origin("off-tool", PluginOrigin::UserClaude);
        disabled.enabled = false;
        let mut state = plugins_modal_state(vec![
            make_plugin_with_origin("user-tool", PluginOrigin::UserEchoAgent),
            disabled,
        ]);
        state.plugins_filter = StatusFilter::Disabled;

        let buf = render_plugins_into_buffer(&mut state, 100, 40);
        assert_eq!(
            buffer_count(&buf, "User (1 plugin)"),
            0,
            "group with no matching plugins must be omitted"
        );
        assert_eq!(buffer_count(&buf, "User (Claude) (1 plugin)"), 1);
        assert_eq!(buffer_count(&buf, "off-tool"), 1);
        assert_eq!(buffer_count(&buf, "[disabled]"), 1);
    }

    #[test]
    fn marketplace_placeholders_render_only_when_expanded() {
        let mut source = superpowers_source();
        source.plugins.truncate(2);
        source.plugins[0].components =
            Some(echo_agent_hooks_plugins_types::PluginComponents::default());
        source.plugins[1].components = None;
        source.plugins[1].skill_count = 0;
        source.plugins[1].has_hooks = false;
        source.plugins[1].has_agents = false;
        source.plugins[1].has_mcp = false;
        assert!(source.plugins[1].remote_url.is_some());
        let mut state = marketplace_modal_state(source);

        let collapsed_buf = render_marketplace_into_buffer(&mut state, 100, 40);
        assert_eq!(
            buffer_count(&collapsed_buf, "no detectable components"),
            0,
            "collapsed row must not show the empty-catalog placeholder"
        );
        assert_eq!(
            buffer_count(&collapsed_buf, "contents shown after install"),
            0,
            "collapsed row must not show the install hint placeholder"
        );

        state.picker_state.expanded.insert(1);
        state.picker_state.expanded.insert(2);
        let expanded_buf = render_marketplace_into_buffer(&mut state, 100, 40);
        assert_eq!(
            buffer_count(&expanded_buf, "no detectable components"),
            1,
            "expanded view shows the empty-catalog placeholder exactly once"
        );
        assert_eq!(
            buffer_count(&expanded_buf, "contents shown after install"),
            1,
            "expanded view shows the install hint placeholder exactly once"
        );
    }

    #[test]
    fn confirmation_overlay_suppresses_managed_url_underline() {
        use crate::views::mcps_modal::McpWireSource;

        // Tall list so the Managed connectors URL sits above the centered
        // confirmation text (not only cells the message string overwrites).
        let mut managed = Vec::new();
        for i in 0..20 {
            managed.push(make_mcp_server_for_rows(
                &format!("echo_agent_com_srv_{i}"),
                McpWireSource::Managed,
                vec![],
            ));
        }
        managed.push(make_mcp_server_for_rows(
            "local-grafana",
            McpWireSource::Local,
            vec![],
        ));

        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.mcps_data = TabDataState::Loaded(managed);
        state.session_team_id = Some("team-1".into());

        let area = Rect::new(0, 0, 100, 40);
        let mut open_buf = Buffer::empty(area);
        render_extensions_modal(&mut open_buf, area, &mut state, None, false, 0);

        let underlined = |buf: &Buffer| -> usize {
            let mut n = 0usize;
            for y in 0..area.height {
                for x in 0..area.width {
                    if buf
                        .cell((x, y))
                        .is_some_and(|c| c.modifier.contains(Modifier::UNDERLINED))
                    {
                        n += 1;
                    }
                }
            }
            n
        };

        assert!(
            underlined(&open_buf) > 0,
            "precondition: managed connectors URL paints UNDERLINED cells"
        );
        assert!(
            state.picker_state.link_band.is_some(),
            "precondition: link hit band recorded for connectors URL"
        );

        state.modal_message = Some(ModalMessage::Confirmation {
            message: "Remove MCP server \"local-grafana\"?".into(),
            action: ConfirmationAction::DeleteMcpServer {
                server_name: "local-grafana".into(),
            },
            pending_entry_index: Some(0),
        });
        state.picker_state.link_band = None;

        let mut confirm_buf = Buffer::empty(area);
        render_extensions_modal(&mut confirm_buf, area, &mut state, None, false, 0);

        assert_eq!(
            buffer_count(&confirm_buf, "Remove MCP server \"local-grafana\"?"),
            1,
            "confirmation message must be painted"
        );
        assert_eq!(
            underlined(&confirm_buf),
            0,
            "confirmation must not paint UNDERLINED under the full overlay"
        );
        assert!(
            state.picker_state.link_band.is_none(),
            "confirmation must not record a connectors link hit band"
        );
    }

    #[test]
    fn first_selectable_index_skips_headers() {
        // Generic list with a non-selectable first row, then two selectable rows.
        let non_sel = [true, false, false];
        assert_eq!(picker::first_selectable_index(0, 3, &non_sel), 1);
        assert_eq!(picker::first_selectable_index(1, 3, &non_sel), 1);
        assert_eq!(picker::first_selectable_index(5, 3, &non_sel), 2); // min-clamp to last
        assert_eq!(picker::first_selectable_index(0, 1, &[true]), 0); // only non-selectable
        assert_eq!(picker::first_selectable_index(0, 0, &[]), 0);
    }

    #[test]
    fn skills_groups_default_collapsed_and_selectable() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Skills);
        let skills = vec![make_skill("alpha", "a"), make_skill("beta", "b")];
        state.seed_skills_groups_once(&skills);
        state.skills_data = TabDataState::Loaded(skills);
        state.picker_state.selected = 0;
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert_eq!(
            state.entry_group_keys.first().and_then(|k| k.as_deref()),
            Some("User"),
            "first row is collapsible User group"
        );
        assert!(
            !state.entry_non_selectable.first().copied().unwrap_or(true),
            "group headers are selectable"
        );
        assert_eq!(
            state
                .entry_data_indices
                .iter()
                .filter(|d| d.is_some())
                .count(),
            0,
            "children hidden while collapsed"
        );
        assert!(
            state
                .entry_labels_cache
                .iter()
                .any(|l| l.starts_with("User (")),
            "collapsed header still visible"
        );
        // Expand User and re-render.
        state.skills_collapsed_groups.remove("User");
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert!(
            state.entry_data_indices.iter().any(|d| d.is_some()),
            "children visible after expand"
        );
    }

    #[test]
    fn skills_tab_renders_no_workflows_rows() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Skills);
        state.skills_data = TabDataState::Loaded(vec![make_skill("alpha", "a")]);
        state.workflows_data = TabDataState::Loaded(vec![WorkflowInfo {
            name: "fix-ci".into(),
            description: "Fix CI".into(),
            when_to_use: None,
            source: "builtin".into(),
            path: None,
        }]);
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert!(
            !state
                .entry_labels_cache
                .iter()
                .any(|l| l.starts_with("Workflows (")),
            "the Skills tab must not render a Workflows group header"
        );
        assert!(
            !state.entry_labels_cache.iter().any(|l| l == "fix-ci"),
            "workflow rows must not render on the Skills tab"
        );
    }

    #[test]
    fn seed_skills_groups_collapses_source_groups() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Skills);
        let skills = vec![make_skill("alpha", "a")];
        state.seed_skills_groups_once(&skills);
        assert!(
            state.skills_collapsed_groups.contains("User"),
            "skills seed collapses skill source groups"
        );
    }

    #[test]
    fn plugins_sort_az_by_name_within_group() {
        use echo_agent_hooks_plugins_types::PluginOrigin;
        let mut state = plugins_modal_state(vec![
            make_plugin_with_origin("Zebra", PluginOrigin::UserEchoAgent),
            make_plugin_with_origin("alpha", PluginOrigin::UserEchoAgent),
            make_plugin_with_origin("MID", PluginOrigin::UserEchoAgent),
        ]);
        let _buf = render_plugins_into_buffer(&mut state, 100, 40);
        assert_eq!(
            state.entry_labels_cache,
            vec![
                "User (3 plugins)".to_string(),
                "alpha".to_string(),
                "MID".to_string(),
                "Zebra".to_string(),
            ]
        );
        // Display order is A–Z; indices still point at original data slots.
        assert_eq!(
            state.entry_data_indices,
            vec![None, Some(1), Some(2), Some(0)]
        );
    }

    #[test]
    fn ordered_marketplace_view_pins_official_then_az() {
        let mp = |name: &str, url: &str, err: Option<&str>, plugins: &[&'static str]| {
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: name.into(),
                source_kind: "git".into(),
                source_url_or_path: url.into(),
                plugins: plugins
                    .iter()
                    .copied()
                    .map(|n| {
                        TestPlugin {
                            name: n,
                            install_status: "installed",
                            ..Default::default()
                        }
                        .build()
                    })
                    .collect(),
                error: err.map(str::to_string),
            }
        };
        let sources = vec![
            mp("zeta-mp", "https://example.com/zeta", Some("boom"), &[]),
            mp(
                "EchoAgent Official",
                echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL,
                None,
                &["zeta", "alpha"],
            ),
            mp("alpha-mp", "https://example.com/alpha", None, &[]),
        ];
        let view = ordered_marketplace_view(&sources);
        let names: Vec<_> = view
            .iter()
            .map(|v| sources[v.source_index].source_name.as_str())
            .collect();
        assert_eq!(names, ["EchoAgent Official", "alpha-mp", "zeta-mp"]);
        let plugin_names: Vec<_> = view[0]
            .plugin_indices
            .iter()
            .map(|&pi| sources[view[0].source_index].plugins[pi].name.as_str())
            .collect();
        assert_eq!(plugin_names, ["alpha", "zeta"]);
    }

    #[test]
    fn skills_groups_then_az_by_label() {
        let mut project_z = make_skill("zzz-proj", "project skill");
        project_z.scope = echo_agent_tools::implementations::skills::types::SkillScope::Local;
        project_z.config_source = Some(
            echo_agent_tools::types::config_source::ConfigSource::Project {
                path: std::path::PathBuf::from("/repo/.echo-agent/skills/zzz"),
            },
        );
        project_z.display_name = Some("zeta-proj".into());

        let mut project_a = make_skill("aaa-proj", "other project");
        project_a.scope = echo_agent_tools::implementations::skills::types::SkillScope::Repo;
        project_a.config_source = Some(
            echo_agent_tools::types::config_source::ConfigSource::Project {
                path: std::path::PathBuf::from("/repo/.echo-agent/skills/aaa"),
            },
        );
        project_a.display_name = Some("alpha-proj".into());

        let mut user_a = make_skill("user-alpha", "user a");
        user_a.display_name = Some("alpha-user".into());
        let plugin = make_plugin_skill("pb", "plugin b", "beta-plugin");

        let skills = vec![user_a, plugin, project_z, project_a];
        let result = filter_and_sort_skills(&skills, "", StatusFilter::All);
        let labels: Vec<_> = result
            .matches
            .iter()
            .map(|m| skills[m.skill_index].label().to_string())
            .collect();
        assert_eq!(labels, ["alpha-proj", "zeta-proj", "alpha-user", "pb"]);

        // Headers come from render; keep one check that they appear.
        let mut state = ExtensionsModalState::new(ExtensionsTab::Skills);
        state.skills_data = TabDataState::Loaded(skills);
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert!(
            state
                .entry_labels_cache
                .iter()
                .any(|l| l.starts_with("Project ("))
        );
        assert!(
            state
                .entry_labels_cache
                .iter()
                .any(|l| l.starts_with("User ("))
        );
        assert!(
            state
                .entry_labels_cache
                .iter()
                .any(|l| l.starts_with("Plugin: beta-plugin ("))
        );
    }

    #[test]
    fn skills_sort_az_uses_label_not_slash_name() {
        let mut a = make_skill("zzz", "desc a");
        a.display_name = Some("aaa".into());
        let b = make_skill("mmm", "desc b");
        let skills = vec![a, b];
        let result = filter_and_sort_skills(&skills, "", StatusFilter::All);
        assert_eq!(skills[result.matches[0].skill_index].label(), "aaa");
        assert_eq!(skills[result.matches[1].skill_index].label(), "mmm");
    }

    #[test]
    fn workflows_sort_az_by_name() {
        let mut state = ExtensionsModalState::new(ExtensionsTab::Workflows);
        state.workflows_data = TabDataState::Loaded(vec![
            WorkflowInfo {
                name: "zeta-wf".into(),
                description: "Z".into(),
                when_to_use: None,
                source: "builtin".into(),
                path: None,
            },
            WorkflowInfo {
                name: "alpha-wf".into(),
                description: "A".into(),
                when_to_use: None,
                source: "builtin".into(),
                path: None,
            },
        ]);
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(&mut buf, area, &mut state, None, false, 0);
        assert_eq!(state.entry_labels_cache, ["alpha-wf", "zeta-wf"]);
    }

    #[test]
    fn hooks_group_and_row_order() {
        let mut h_stop = make_hook("stop-hook", "/tmp/hooks-src", false);
        h_stop.event = echo_agent_hooks_plugins_types::HookEvent::Stop;
        let mut h_pre = make_hook("pre-hook", "/tmp/hooks-src", false);
        h_pre.event = echo_agent_hooks_plugins_types::HookEvent::PreToolUse;
        h_pre.matcher = Some("Bash".into());
        let mut h_notify = make_hook("notify-hook", "/tmp/hooks-src", false);
        h_notify.event = echo_agent_hooks_plugins_types::HookEvent::Notification;

        let hooks = vec![
            make_hook("c", "/zzz/custom", false),
            h_stop,
            make_hook("a", "/repo/.echo-agent/hooks", false),
            h_pre,
            h_notify,
            make_hook("b", "/aaa/custom", false),
        ];
        let groups = build_hook_groups(&hooks, StatusFilter::All, "");
        // Project first, then Custom groups A–Z by path (incl. /tmp/hooks-src).
        let dirs: Vec<_> = groups.iter().map(|g| g.source_dir).collect();
        assert_eq!(
            dirs,
            [
                "/repo/.echo-agent/hooks",
                "/aaa/custom",
                "/tmp/hooks-src",
                "/zzz/custom"
            ]
        );

        let custom_group = groups
            .iter()
            .find(|g| g.source_dir == "/tmp/hooks-src")
            .expect("custom source");
        let labels: Vec<_> = custom_group
            .indices
            .iter()
            .map(|&i| hook_row_label(&hooks[i]))
            .collect();
        assert_eq!(
            labels,
            ["on:Notification", "on:Pre-Tool Use /Bash", "on:Stop"]
        );
    }
}
