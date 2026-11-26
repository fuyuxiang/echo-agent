//! `list_dir` tool — new architecture (`Tool` trait).
//!
//! This is the new-architecture implementation of the directory listing tool.
//! It reads `Cwd` from `Resources` and `max_output_chars` from its own
//! `Params<ListDirParams>` instead of receiving them via `ToolContext`.
//!
//! Seeds depth-1 children (capped at `MAX_SEED_ITEMS`) before the budgeted deep walk
//! so a fat early sibling cannot starve later top-level dirs (`MAX_GLOBAL_ITEMS`
//! applies only to depth ≥ 2). Then BFS-expands dirs within the char budget
//! (`continue` on fat dirs). When either seed or walk hits its item limit, the
//! agent-visible cutoff notice is emitted (copy unchanged from `main`).
//!
//! Partial-output case: when `walk_truncated` is true, a sibling surfaced by the
//! seed may be listed by name only while its descendants are absent (the walk cap
//! was exhausted inside an earlier sibling). The agent-visible notice copy is
//! intentionally left identical to `main`; this behavior is documented in the
//! CHANGELOG rather than via new model-facing wording.
//!
//! Under `legacy-0.4.10`, the old depth-threshold algorithm is used instead
//! (see `versions::legacy_0_4_10` module).
mod versions;
use crate::types::output::{ListDirContent, ListDirOutput};
#[allow(unused_imports)]
use crate::types::resources::{
    DisplayCwd, Params, PathNotFoundHints, RespectGitignore, SharedResources, display_cwd_or_cwd,
    resolve_model_path,
};
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::{ToolKind, ToolNamespace};
use std::collections::HashMap;
use std::path::Path;
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ListDirInput {
    #[schemars(
        description = "Path to directory to list contents of, relative to the workspace root or absolute."
    )]
    pub target_directory: String,
}
/// Per-tool configuration for `list_dir`, stored as `Params<ListDirParams>`
/// in Resources. Set via `SetToolOptions` / gRPC or at registration time.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListDirParams {
    /// BFS expansion stops when this budget would be exceeded.
    /// Defaults to `DEFAULT_MAX_OUTPUT_CHARS` (10,000) to match the Python
    pub max_output_chars: Option<usize>,
}
crate::register_resource!("echo_agent_build", "ListDir", ListDirParams);
/// Exact historical invalid-directory message for `list_dir` in legacy-0.4.10.
///
/// Historical fixture captured from an earlier (0.4.10) revision of this tool.
///
/// Historical 0.4.10 collapsed nonexistent paths, file paths, and other
/// invalid-directory failures into the same generic message.
fn render_legacy_list_dir_error(path: &Path) -> String {
    format!("Error: {} is not a valid directory", path.display())
}
/// Internal version discriminant for list_dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListDirVersion {
    Current,
    Legacy0_4_10,
}
impl ListDirVersion {
    pub(crate) fn from_contract(v: Option<&str>) -> Self {
        match v {
            Some("legacy-0.4.10") => Self::Legacy0_4_10,
            _ => Self::Current,
        }
    }
    pub(crate) fn is_legacy(self) -> bool {
        self == Self::Legacy0_4_10
    }
}
/// Compute the path shown in the list_dir tool result header.
/// Special-cases `list_dir(".")`, `list_dir("")`, and `list_dir("./foo")` so the
/// output does not contain ugly "/./" components (e.g. `/workspace/./`).
fn compute_display_path(display_base: &std::path::Path, target: &str) -> std::path::PathBuf {
    let t = target.trim().trim_start_matches("./");
    if t.is_empty() || t == "." {
        display_base.to_path_buf()
    } else {
        display_base.join(t)
    }
}
#[derive(Debug, Default)]
pub struct ListDirTool;
/// Default character budget for directory listing output.
/// Matches the Python SWE tool's `lim_characters` default.
const DEFAULT_MAX_OUTPUT_CHARS: usize = 10_000;
/// Show top-N extension buckets in collapsed-directory summary lines.
const TOP_K_EXTENSIONS: usize = 3;
/// Notice appended when the root directory's immediate children exceed the
/// character budget. Tool names are resolved via [`TemplateRenderer`].
const ROOT_TRUNCATION_NOTICE_TEMPLATE: &str = "    ...\n\n\
    Note: this directory is too large to list fully. Try ${{ tools.by_kind.list }} on a \
    narrower path, or use ${{ tools.by_kind.search }} / ${{ tools.by_kind.execute }}.";
/// Fallback when no [`TemplateRenderer`] is available (unit tests).
const ROOT_TRUNCATION_NOTICE_FALLBACK: &str = "    ...\n\n\
    Note: this directory is too large to list fully. Try list_dir on a narrower path, or \
    use grep / bash.";
fn root_truncation_notice(renderer: Option<&TemplateRenderer>) -> String {
    renderer
        .and_then(|r| r.render(ROOT_TRUNCATION_NOTICE_TEMPLATE).ok())
        .unwrap_or_else(|| ROOT_TRUNCATION_NOTICE_FALLBACK.to_string())
}
/// Hard cap on deep-walk (depth ≥ 2) items; depth-1 seed is not counted. Matches
/// the Python SWE tool's `MAX_GLOBAL_ITEMS`.
const MAX_GLOBAL_ITEMS: usize = 100_000;
/// Cap on depth-1 seed entries so a pathological flat root (millions of direct
/// children) cannot fully materialize into `DirNode` before the char budget truncates.
/// Independent in role from `MAX_GLOBAL_ITEMS`, but pinned equal to it (see guard below) so
/// the cutoff notice's shared count stays correct whichever cap triggers truncation.
const MAX_SEED_ITEMS: usize = 100_000;
const _: () = assert!(MAX_SEED_ITEMS == MAX_GLOBAL_ITEMS);
#[derive(Debug, Default)]
struct DirAccum {
    total_files: usize,
    by_ext: HashMap<String, usize>,
}
impl DirAccum {
    fn add_ext(&mut self, ext: &str) {
        self.total_files += 1;
        *self.by_ext.entry(ext.to_owned()).or_default() += 1;
    }
    /// Render a summary like `[3 files in subtree: 2 *.rs, 1 *.toml]`.
    fn to_summary(&self, top_n: usize) -> String {
        if self.by_ext.is_empty() {
            return String::new();
        }
        let mut items: Vec<(String, usize)> =
            self.by_ext.iter().map(|(k, v)| (k.clone(), *v)).collect();
        items.sort_by(|a, b| match b.1.cmp(&a.1) {
            std::cmp::Ordering::Equal => a.0.cmp(&b.0),
            other => other,
        });
        let mut parts: Vec<String> = Vec::new();
        let mut top_sum: usize = 0;
        for (ext, count) in items.iter().take(top_n) {
            top_sum += *count;
            if ext == "no-ext" {
                parts.push(format!("{} *no-ext", count));
            } else {
                parts.push(format!("{} *.{}", count, ext));
            }
        }
        let ellipsis = if top_sum < self.total_files {
            ", ..."
        } else {
            ""
        };
        let file_word = if self.total_files == 1 {
            "file"
        } else {
            "files"
        };
        format!(
            "[{} {} in subtree: {}{}]",
            self.total_files,
            file_word,
            parts.join(", "),
            ellipsis
        )
    }
}
fn ext_key_from_path(path: &Path) -> String {
    path.extension().map_or("no-ext".to_string(), |s| {
        s.to_string_lossy().to_ascii_lowercase()
    })
}
#[derive(Debug)]
struct DirNode {
    depth: usize,
    files: Vec<String>,
    subdirs: Vec<String>,
    children: HashMap<String, DirNode>,
    subtree: DirAccum,
    is_expanded: bool,
}
impl DirNode {
    fn new(depth: usize) -> Self {
        Self {
            depth,
            files: Vec::new(),
            subdirs: Vec::new(),
            children: HashMap::new(),
            subtree: DirAccum::default(),
            is_expanded: false,
        }
    }
    fn add_item(&mut self, rel_parts: &[&str], is_dir: bool) {
        if rel_parts.is_empty() {
            return;
        }
        if rel_parts.len() == 1 {
            let name = rel_parts[0].to_owned();
            if is_dir {
                let key = format!("{name}/");
                if !self.children.contains_key(&key) {
                    self.children
                        .insert(key.clone(), DirNode::new(self.depth + 1));
                    self.subdirs.push(key);
                }
            } else {
                let ext = ext_key_from_path(Path::new(&name));
                self.files.push(name);
                self.subtree.add_ext(&ext);
            }
            return;
        }
        let subdir = rel_parts[0];
        let key = format!("{subdir}/");
        if !self.children.contains_key(&key) {
            self.children
                .insert(key.clone(), DirNode::new(self.depth + 1));
            self.subdirs.push(key.clone());
        }
        let child = self.children.get_mut(&key).expect("just inserted");
        child.add_item(&rel_parts[1..], is_dir);
        if !is_dir {
            let ext = ext_key_from_path(Path::new(rel_parts.last().unwrap()));
            self.subtree.add_ext(&ext);
        }
    }
    /// Sort files and subdirs case-insensitively, recursively.
    fn sort_recursive(&mut self) {
        self.files.sort_by_key(|a| a.to_ascii_lowercase());
        self.subdirs.sort_by_key(|a| a.to_ascii_lowercase());
        for child in self.children.values_mut() {
            child.sort_recursive();
        }
    }
    fn all_subitems_sorted(&self) -> Vec<&str> {
        let mut items: Vec<&str> = self
            .files
            .iter()
            .map(String::as_str)
            .chain(self.subdirs.iter().map(String::as_str))
            .collect();
        items.sort_by_key(|a| a.to_ascii_lowercase());
        items
    }
    fn subitem_line(&self, name: &str) -> String {
        let indent = "  ".repeat(self.depth + 1);
        format!("{indent}- {name}")
    }
    fn summary_str(&self, top_k: usize) -> String {
        self.subtree.to_summary(top_k)
    }
    fn summary_char_cost(&self, top_k: usize) -> usize {
        let s = self.summary_str(top_k);
        if s.is_empty() {
            return 0;
        }
        (self.depth + 1) * 2 + s.len() + 1
    }
    /// Render this node's children, recursing into expanded child nodes.
    fn render_expanded(&self, top_k: usize) -> String {
        let mut out = String::new();
        for name in self.all_subitems_sorted() {
            out.push_str(&self.subitem_line(name));
            out.push('\n');
            if let Some(child) = self.children.get(name) {
                out.push_str(&child.render_subtree(top_k));
            }
        }
        out
    }
    /// Render subtree: expanded nodes show children, collapsed show summary.
    fn render_subtree(&self, top_k: usize) -> String {
        if self.is_expanded {
            return self.render_expanded(top_k);
        }
        let summary = self.summary_str(top_k);
        if summary.is_empty() {
            return String::new();
        }
        let indent = "  ".repeat(self.depth + 1);
        format!("{indent}{summary}\n")
    }
}
/// Shared `WalkBuilder` for seed and deep walk (same `RespectGitignore` flags).
fn list_dir_walk_builder(root: &Path, respect_gitignore: bool) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .standard_filters(true)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore);
    builder
}
/// Seed depth-1 before budgeted walk so later siblings survive `MAX_GLOBAL_ITEMS` starvation.
/// Returns `true` if the seed hit `max_seed` (more depth-1 entries exist).
fn seed_depth1_children(
    root: &Path,
    root_node: &mut DirNode,
    respect_gitignore: bool,
    max_seed: usize,
) -> bool {
    let walker = list_dir_walk_builder(root, respect_gitignore)
        .max_depth(Some(1))
        .build();
    let mut seed_count: usize = 0;
    for entry in walker {
        let Ok(entry) = entry else { continue };
        if entry.depth() != 1 {
            continue;
        }
        let Some(ft) = entry.file_type() else {
            continue;
        };
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        seed_count += 1;
        if seed_count > max_seed {
            return true;
        }
        root_node.add_item(&[name], ft.is_dir());
    }
    false
}
/// Depth-1 seed first, then deep walk; only depth ≥ 2 counts toward `max_items`.
fn build_tree(root: &Path, respect_gitignore: bool) -> (DirNode, bool) {
    build_tree_with_limit(root, respect_gitignore, MAX_GLOBAL_ITEMS)
}
fn build_tree_with_limit(
    root: &Path,
    respect_gitignore: bool,
    max_items: usize,
) -> (DirNode, bool) {
    let mut root_node = DirNode::new(0);
    let seed_truncated =
        seed_depth1_children(root, &mut root_node, respect_gitignore, MAX_SEED_ITEMS);
    let walker = list_dir_walk_builder(root, respect_gitignore).build();
    let mut item_count: usize = 0;
    let mut walk_truncated = false;
    for entry in walker {
        let Ok(entry) = entry else { continue };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if entry.depth() <= 1 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
        if parts.is_empty() {
            continue;
        }
        item_count += 1;
        if item_count > max_items {
            walk_truncated = true;
            break;
        }
        root_node.add_item(&parts, ft.is_dir());
    }
    root_node.sort_recursive();
    (root_node, seed_truncated || walk_truncated)
}
/// BFS-expand directories within the character budget, return rendered body.
fn budget_expand(
    root: &mut DirNode,
    max_chars: usize,
    top_k: usize,
    truncated: bool,
    truncation_notice: &str,
) -> String {
    let cutoff_msg = if truncated {
        format!(
            "\nNote: there are more than {} items in the directory, \
             so not all files may be shown.\n",
            MAX_GLOBAL_ITEMS
        )
    } else {
        String::new()
    };
    if root.files.is_empty() && root.subdirs.is_empty() {
        return cutoff_msg;
    }
    root.is_expanded = true;
    let root_expanded = root.render_expanded(top_k);
    if root_expanded.len() > max_chars {
        tracing::debug!(
            root_chars = root_expanded.len(),
            budget = max_chars,
            "list_dir root children exceed budget, truncating"
        );
        let mut out = render_truncated_root(root, max_chars, top_k, truncation_notice);
        out.push_str(&cutoff_msg);
        return out;
    }
    let mut remaining = max_chars - root_expanded.len();
    let mut queue: std::collections::VecDeque<Vec<String>> = std::collections::VecDeque::new();
    for name in &root.subdirs {
        queue.push_back(vec![name.clone()]);
    }
    while let Some(node_path) = queue.pop_front() {
        let Some(node) = navigate_mut(root, &node_path) else {
            continue;
        };
        node.is_expanded = true;
        let expanded = node.render_expanded(top_k);
        let summary_cost = node.summary_char_cost(top_k);
        if expanded.len() > remaining + summary_cost {
            node.is_expanded = false;
            continue;
        }
        remaining += summary_cost;
        remaining -= expanded.len();
        let child_names: Vec<String> = node.subdirs.clone();
        for child_name in child_names {
            let mut child_path = node_path.clone();
            child_path.push(child_name);
            queue.push_back(child_path);
        }
    }
    let mut out = root.render_expanded(top_k);
    out.push_str(&cutoff_msg);
    out
}
fn navigate_mut<'a>(root: &'a mut DirNode, path: &[String]) -> Option<&'a mut DirNode> {
    let mut node = root;
    for key in path {
        node = node.children.get_mut(key)?;
    }
    Some(node)
}
/// Render as many root items as fit within budget, then append truncation notice.
fn render_truncated_root(root: &DirNode, max_chars: usize, top_k: usize, notice: &str) -> String {
    let mut out = String::new();
    let mut remaining = max_chars;
    let child_summary_indent = "  ".repeat(root.depth + 2);
    for name in root.all_subitems_sorted() {
        let mut chunk = root.subitem_line(name);
        chunk.push('\n');
        if let Some(child) = root.children.get(name) {
            let summary = child.summary_str(top_k);
            if !summary.is_empty() {
                chunk.push_str(&format!("{child_summary_indent}{summary}\n"));
            }
        }
        if chunk.len() > remaining {
            break;
        }
        out.push_str(&chunk);
        remaining -= chunk.len();
    }
    out.push_str(notice);
    out
}
/// Walk inputs captured before `spawn_blocking`. The Resources mutex is
/// async and cannot be locked on the blocking thread.
enum ListDirWalk {
    Legacy {
        max_output_bytes: usize,
    },
    Current {
        max_output_chars: usize,
        respect_gitignore: bool,
        truncation_notice: String,
    },
}
fn map_list_dir_join_error(
    err: tokio::task::JoinError,
    display_path: &Path,
) -> echo_agent_tool_runtime::ToolError {
    let tool_id = echo_agent_tool_protocol::ToolId::new("list_dir").expect("valid tool id");
    let path = display_path.display();
    if err.is_cancelled() {
        tracing::debug!(error = %err, "list_dir walk task cancelled");
        echo_agent_tool_runtime::ToolError::cancelled(
            tool_id,
            format!("Directory listing was cancelled for {path}."),
        )
    } else {
        tracing::warn!(error = %err, "list_dir walk task panicked");
        echo_agent_tool_runtime::ToolError::execution(
            tool_id,
            format!(
                "Directory listing failed unexpectedly for {path}; retry or narrow the target directory."
            ),
        )
    }
}
/// Offload a sync directory walk. A panic on the blocking thread becomes a
/// tool error so it cannot abort the session's current-thread runtime.
async fn spawn_list_dir_walk<T, F>(
    display_path: &Path,
    walk: F,
) -> Result<T, echo_agent_tool_runtime::ToolError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(walk)
        .await
        .map_err(|err| map_list_dir_join_error(err, display_path))
}
impl echo_agent_tool_runtime::Tool for ListDirTool {
    type Args = ListDirInput;
    type Output = ListDirOutput;
    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("list_dir").expect("valid tool id")
    }
    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "list_dir",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }
    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }
    #[tracing::instrument(
        name = "tool.list_dir",
        skip_all,
        fields(path = %input.target_directory)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: ListDirInput,
    ) -> Result<ListDirOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::{behavior_version, resolve_cwd, shared_resources};
        let resources = shared_resources(&ctx)?;
        let cwd = resolve_cwd(&ctx, &resources).await?;
        let is_legacy =
            ListDirVersion::from_contract(behavior_version(&ctx).as_deref()).is_legacy();
        let (display_cwd, hints_enabled) = {
            let res = resources.lock().await;
            (
                res.get::<DisplayCwd>().map(|d| d.0.clone()),
                res.get::<PathNotFoundHints>().is_some_and(|h| h.0),
            )
        };
        let path = resolve_model_path(&cwd, display_cwd.as_deref(), &input.target_directory);
        let display_base = display_cwd_or_cwd(&cwd, display_cwd.as_deref());
        let display_path = compute_display_path(&display_base, &input.target_directory);
        let meta = tokio::fs::metadata(&path).await;
        let is_dir = meta.as_ref().is_ok_and(|m| m.is_dir());
        if !is_dir {
            if is_legacy {
                return Ok(ListDirOutput::Error(render_legacy_list_dir_error(
                    &display_path,
                )));
            }
            return Ok(match &meta {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let msg = crate::util::format_not_found_error(
                        &display_path,
                        &path,
                        &cwd,
                        &display_base,
                        hints_enabled,
                    )
                    .await;
                    ListDirOutput::NotFound(msg)
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    ListDirOutput::PermissionDenied(format!(
                        "Permission denied: {}",
                        display_path.display()
                    ))
                }
                Ok(m) if m.is_file() => ListDirOutput::IsAFile(format!(
                    "Error: {} is a file, not a directory.",
                    display_path.display()
                )),
                _ => ListDirOutput::NotADirectory(format!(
                    "Error: {} is not a valid directory.",
                    display_path.display()
                )),
            });
        }
        let walk = if is_legacy {
            let max_output_bytes = resources
                .lock()
                .await
                .get::<Params<ListDirParams>>()
                .and_then(|p| p.0.max_output_chars)
                .unwrap_or(crate::DEFAULT_TOOL_OUTPUT_BYTES);
            ListDirWalk::Legacy { max_output_bytes }
        } else {
            let res = resources.lock().await;
            let max_output_chars = res
                .get::<Params<ListDirParams>>()
                .and_then(|p| p.0.max_output_chars)
                .unwrap_or(DEFAULT_MAX_OUTPUT_CHARS);
            let respect_gitignore = res.get::<RespectGitignore>().is_none_or(|r| r.0);
            let truncation_notice = root_truncation_notice(res.get::<TemplateRenderer>());
            ListDirWalk::Current {
                max_output_chars,
                respect_gitignore,
                truncation_notice,
            }
        };
        let (body, path) = spawn_list_dir_walk(&display_path, move || {
            let body = match walk {
                ListDirWalk::Legacy { max_output_bytes } => {
                    versions::legacy_0_4_10::render_legacy(&path, max_output_bytes)
                }
                ListDirWalk::Current {
                    max_output_chars,
                    respect_gitignore,
                    truncation_notice,
                } => {
                    let (mut tree, truncated) = build_tree(&path, respect_gitignore);
                    budget_expand(
                        &mut tree,
                        max_output_chars,
                        TOP_K_EXTENSIONS,
                        truncated,
                        &truncation_notice,
                    )
                }
            };
            (body, path)
        })
        .await?;
        let trimmed_body = body.trim_end();
        let output = if trimmed_body.is_empty() && is_legacy {
            format!("- {}/\n  no children found", display_path.display())
        } else {
            format!("- {}/\n{}", display_path.display(), trimmed_body)
        };
        Ok(ListDirOutput::Content(ListDirContent {
            content: output,
            absolute_root_path: path,
        }))
    }
}
impl crate::types::tool_metadata::ToolMetadata for ListDirTool {
    fn kind(&self) -> ToolKind {
        ToolKind::List
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }
    fn description_template(&self) -> &str {
        r#"Lists files and directories in a given path.
The '${{ params.list.target_directory }}' parameter can be relative to the workspace root or absolute.

Other details:
    - The result does not display dot-files and dot-directories.
    - Respects .gitignore patterns (files/directories ignored by git are not shown).
    - Large directories are summarized with file counts and extension breakdowns instead of listing all files."#
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::resources::{Cwd, Resources};
    use crate::types::tool_metadata::test_ctx;
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    /// Minimal `.git` worktree so the `ignore` crate applies `.gitignore` without a `git` binary
    /// (some CI environments may not have `git` on PATH).
    fn init_minimal_git_worktree(root: &Path) {
        let git_dir = root.join(".git");
        fs::create_dir_all(git_dir.join("objects")).unwrap();
        fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/master\n").unwrap();
        fs::write(
            git_dir.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = false\n",
        )
        .unwrap();
    }
    #[test]
    fn compute_display_path_dot_returns_base() {
        let base = std::path::Path::new("/workspace");
        assert_eq!(compute_display_path(base, "."), PathBuf::from("/workspace"));
    }
    #[test]
    fn compute_display_path_empty_returns_base() {
        let base = std::path::Path::new("/workspace");
        assert_eq!(compute_display_path(base, ""), PathBuf::from("/workspace"));
        assert_eq!(
            compute_display_path(base, "   "),
            PathBuf::from("/workspace")
        );
    }
    #[test]
    fn compute_display_path_dot_slash_is_treated_as_dot() {
        let base = std::path::Path::new("/workspace");
        assert_eq!(
            compute_display_path(base, "./"),
            PathBuf::from("/workspace")
        );
    }
    #[test]
    fn compute_display_path_normal_relative_is_joined() {
        let base = std::path::Path::new("/workspace");
        assert_eq!(
            compute_display_path(base, "src/utils"),
            PathBuf::from("/workspace/src/utils")
        );
    }
    #[test]
    fn small_deep_dirs_are_expanded() {
        let tmp = TempDir::new().unwrap();
        let deep = tmp.path().join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();
        File::create(deep.join("deep.rs")).unwrap();
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.contains("deep.rs"),
            "deep file should be visible: {body}"
        );
        assert!(body.contains("- a/"), "dir a should be listed");
        assert!(body.contains("- b/"), "dir b should be listed");
        assert!(body.contains("- c/"), "dir c should be listed");
    }
    #[test]
    fn large_directory_is_summarized_by_budget() {
        let tmp = TempDir::new().unwrap();
        let subdir = tmp.path().join("big");
        fs::create_dir(&subdir).unwrap();
        for i in 0..50 {
            File::create(subdir.join(format!("file{}.rs", i))).unwrap();
        }
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            200,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(body.contains("- big/"), "subdir should appear");
        assert!(
            body.contains("[50 files in subtree: 50 *.rs]"),
            "subdir should be collapsed to summary: {body}"
        );
        assert!(
            !body.contains("file0.rs"),
            "individual files should not appear"
        );
    }
    #[test]
    fn empty_directory_renders_nothing() {
        let tmp = TempDir::new().unwrap();
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.is_empty(),
            "empty dir should produce empty body: {body}"
        );
    }
    #[test]
    fn hidden_files_are_excluded() {
        let tmp = TempDir::new().unwrap();
        File::create(tmp.path().join(".hidden")).unwrap();
        File::create(tmp.path().join(".secret.txt")).unwrap();
        File::create(tmp.path().join("visible.rs")).unwrap();
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(body.contains("visible.rs"));
        assert!(!body.contains(".hidden"));
        assert!(!body.contains(".secret"));
    }
    #[test]
    fn budget_truncation_shows_notice() {
        let tmp = TempDir::new().unwrap();
        for i in 0..30 {
            let subdir = tmp.path().join(format!("dir_{:02}", i));
            fs::create_dir(&subdir).unwrap();
            for j in 0..10 {
                File::create(subdir.join(format!("file_{}.rs", j))).unwrap();
            }
        }
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            200,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.contains("too large to list fully"),
            "should contain truncation notice: {body}"
        );
    }
    #[test]
    fn root_truncation_notice_renders_tool_names() {
        let tools: HashMap<ToolKind, String> = [
            (ToolKind::List, "list_dir".to_string()),
            (ToolKind::Search, "grep".to_string()),
            (ToolKind::Execute, "bash".to_string()),
        ]
        .into_iter()
        .collect();
        let renderer = TemplateRenderer::new(tools, HashMap::new());
        let notice = root_truncation_notice(Some(&renderer));
        assert!(notice.contains("list_dir"));
        assert!(notice.contains("grep"));
        assert!(notice.contains("bash"));
        assert!(
            !notice.contains("${{"),
            "template markers should be resolved: {notice}"
        );
    }
    #[test]
    fn walk_truncation_shows_cutoff_message() {
        let tmp = TempDir::new().unwrap();
        File::create(tmp.path().join("file.rs")).unwrap();
        let (mut tree, _) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            true,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.contains("more than 100000 items"),
            "should contain cutoff notice: {body}"
        );
    }
    #[test]
    fn bfs_expands_breadth_first() {
        let tmp = TempDir::new().unwrap();
        let dir_a = tmp.path().join("aaa");
        let dir_b = tmp.path().join("bbb");
        fs::create_dir(&dir_a).unwrap();
        fs::create_dir(&dir_b).unwrap();
        File::create(dir_a.join("a.rs")).unwrap();
        File::create(dir_b.join("b.rs")).unwrap();
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(body.contains("a.rs"), "a.rs should be visible: {body}");
        assert!(body.contains("b.rs"), "b.rs should be visible: {body}");
    }
    #[test]
    fn budget_collapses_last_oversized_sibling() {
        let tmp = TempDir::new().unwrap();
        let small = tmp.path().join("aaa_small");
        fs::create_dir(&small).unwrap();
        File::create(small.join("s.rs")).unwrap();
        let big = tmp.path().join("bbb_big");
        fs::create_dir(&big).unwrap();
        for i in 0..50 {
            File::create(big.join(format!("f{}.rs", i))).unwrap();
        }
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            400,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.contains("s.rs"),
            "small dir should be expanded: {body}"
        );
        assert!(
            body.contains("[50 files in subtree:"),
            "big dir should remain collapsed: {body}"
        );
    }
    #[test]
    fn budget_skips_big_dir_and_expands_later_small_sibling() {
        let tmp = TempDir::new().unwrap();
        let big = tmp.path().join("aaa_big");
        fs::create_dir(&big).unwrap();
        for i in 0..50 {
            File::create(big.join(format!("f{}.rs", i))).unwrap();
        }
        let small = tmp.path().join("zzz_small");
        fs::create_dir(&small).unwrap();
        File::create(small.join("s.rs")).unwrap();
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            200,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.contains("s.rs"),
            "late small dir should be expanded despite earlier big dir: {body}"
        );
        assert!(
            body.contains("[50 files in subtree:"),
            "big dir should remain collapsed: {body}"
        );
        let big_idx = body.find("- aaa_big/").expect("aaa_big listed");
        let small_idx = body.find("- zzz_small/").expect("zzz_small listed");
        assert!(
            big_idx < small_idx,
            "alphabetical ordering must hold (aaa_big before zzz_small): {body}"
        );
    }
    #[test]
    fn budget_skips_multiple_oversized_siblings_interleaved() {
        const BUDGET: usize = 300;
        let tmp = TempDir::new().unwrap();
        let aaa = tmp.path().join("aaa_big");
        let ccc = tmp.path().join("ccc_big");
        fs::create_dir(&aaa).unwrap();
        fs::create_dir(&ccc).unwrap();
        for i in 0..50 {
            File::create(aaa.join(format!("f{}.rs", i))).unwrap();
            File::create(ccc.join(format!("g{}.rs", i))).unwrap();
        }
        let bbb = tmp.path().join("bbb_small");
        let ddd = tmp.path().join("ddd_small");
        fs::create_dir(&bbb).unwrap();
        fs::create_dir(&ddd).unwrap();
        File::create(bbb.join("s1.rs")).unwrap();
        File::create(ddd.join("s2.rs")).unwrap();
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            BUDGET,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.contains("s1.rs"),
            "bbb_small file should be shown: {body}"
        );
        assert!(
            body.contains("s2.rs"),
            "ddd_small file should be shown: {body}"
        );
        assert!(
            !body.contains("f0.rs"),
            "aaa_big files must stay collapsed: {body}"
        );
        assert!(
            !body.contains("g0.rs"),
            "ccc_big files must stay collapsed: {body}"
        );
        assert_eq!(
            body.matches("[50 files in subtree:").count(),
            2,
            "both big dirs should collapse to summaries: {body}"
        );
        let i_aaa = body.find("- aaa_big/").expect("aaa_big listed");
        let i_bbb = body.find("- bbb_small/").expect("bbb_small listed");
        let i_ccc = body.find("- ccc_big/").expect("ccc_big listed");
        let i_ddd = body.find("- ddd_small/").expect("ddd_small listed");
        assert!(
            i_aaa < i_bbb && i_bbb < i_ccc && i_ccc < i_ddd,
            "alphabetical ordering must hold: {body}"
        );
        assert!(
            body.len() <= BUDGET,
            "output ({}) must not exceed budget ({BUDGET}): {body}",
            body.len()
        );
    }
    #[test]
    fn seed_cap_truncation_shows_cutoff_message() {
        const SEED_LIMIT: usize = 3;
        let tmp = TempDir::new().unwrap();
        for i in 0..10 {
            File::create(tmp.path().join(format!("f{}.rs", i))).unwrap();
        }
        let mut root_node = DirNode::new(0);
        let truncated = seed_depth1_children(tmp.path(), &mut root_node, true, SEED_LIMIT);
        assert!(truncated, "10 depth-1 entries should exceed seed cap of 3");
        root_node.sort_recursive();
        let body = budget_expand(
            &mut root_node,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            truncated,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.contains("more than 100000 items"),
            "seed cap should surface cutoff notice: {body}"
        );
        assert!(
            body.contains("so not all files may be shown."),
            "cutoff copy unchanged from main: {body}"
        );
        assert!(
            body.trim_end().ends_with("so not all files may be shown."),
            "no extra sentence appended after the main cutoff copy: {body}"
        );
    }
    #[test]
    fn depth1_seed_survives_real_walk_truncation() {
        let tmp = TempDir::new().unwrap();
        let aaa = tmp.path().join("aaa");
        let zzz = tmp.path().join("zzz");
        fs::create_dir(&aaa).unwrap();
        fs::create_dir(&zzz).unwrap();
        for i in 0..30 {
            File::create(aaa.join(format!("f{}.rs", i))).unwrap();
        }
        File::create(zzz.join("late.rs")).unwrap();
        const WALK_LIMIT: usize = 5;
        let (mut tree, truncated) = build_tree_with_limit(tmp.path(), true, WALK_LIMIT);
        assert!(truncated, "30 depth≥2 files should exceed limit of 5");
        assert!(
            tree.subdirs.iter().any(|s| s == "zzz/"),
            "seed must include zzz/ despite walk truncation: {:?}",
            tree.subdirs
        );
        let body = budget_expand(
            &mut tree,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            truncated,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(body.contains("- zzz/"), "zzz/ must appear: {body}");
        assert!(body.contains("- aaa/"), "aaa/ must appear: {body}");
        assert!(
            body.contains("more than 100000 items"),
            "agent-visible cutoff copy unchanged from main: {body}"
        );
    }
    #[test]
    fn depth1_seed_via_build_tree_includes_all_siblings() {
        let tmp = TempDir::new().unwrap();
        let early = tmp.path().join("aaa_mono");
        let late = tmp.path().join("zzz_target");
        fs::create_dir(&early).unwrap();
        fs::create_dir(&late).unwrap();
        for i in 0..30 {
            File::create(early.join(format!("deep_{}.rs", i))).unwrap();
        }
        File::create(late.join("marker.rs")).unwrap();
        File::create(tmp.path().join("readme.md")).unwrap();
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(body.contains("- aaa_mono/"), "early dir: {body}");
        assert!(body.contains("- zzz_target/"), "late dir: {body}");
        assert!(body.contains("readme.md"), "root file: {body}");
        assert!(
            body.contains("marker.rs"),
            "late small dir should expand: {body}"
        );
        let readme_lines: Vec<_> = body.lines().filter(|l| l.contains("readme.md")).collect();
        assert_eq!(
            readme_lines.len(),
            1,
            "depth-1 file must not be duplicated by seed+walk: {body}"
        );
    }
    #[test]
    fn depth1_seed_includes_gitignored_when_respect_gitignore_false() {
        let tmp = TempDir::new().unwrap();
        init_minimal_git_worktree(tmp.path());
        File::create(tmp.path().join("kept.txt")).unwrap();
        File::create(tmp.path().join("ignored.log")).unwrap();
        fs::create_dir(tmp.path().join("zzz_ignored")).unwrap();
        File::create(tmp.path().join("zzz_ignored").join("inner.rs")).unwrap();
        fs::write(tmp.path().join(".gitignore"), "*.log\nzzz_ignored/\n").unwrap();
        let (mut tree_off, trunc_off) = build_tree(tmp.path(), false);
        let body_off = budget_expand(
            &mut tree_off,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            trunc_off,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body_off.contains("ignored.log"),
            "respect_gitignore=false must seed gitignored file: {body_off}"
        );
        assert!(
            body_off.contains("- zzz_ignored/"),
            "respect_gitignore=false must seed gitignored dir: {body_off}"
        );
        let (mut tree_on, trunc_on) = build_tree(tmp.path(), true);
        let body_on = budget_expand(
            &mut tree_on,
            DEFAULT_MAX_OUTPUT_CHARS,
            TOP_K_EXTENSIONS,
            trunc_on,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            !body_on.contains("ignored.log"),
            "respect_gitignore=true must hide gitignored file: {body_on}"
        );
        assert!(
            !body_on.contains("zzz_ignored"),
            "respect_gitignore=true must hide gitignored dir: {body_on}"
        );
        assert!(
            body_on.contains("kept.txt"),
            "non-ignored file still shown under respect_gitignore=true: {body_on}"
        );
    }
    #[test]
    fn fat_gate_at_level_order_preserves_small_sibling_file() {
        const BUDGET: usize = 400;
        let tmp = TempDir::new().unwrap();
        let images = tmp.path().join("images");
        fs::create_dir(&images).unwrap();
        for i in 0..80 {
            File::create(images.join(format!("img_{:03}.jpg", i))).unwrap();
        }
        let scripts = tmp.path().join("scripts");
        fs::create_dir(&scripts).unwrap();
        File::create(scripts.join("one.py")).unwrap();
        let (mut tree, trunc) = build_tree(tmp.path(), true);
        let body = budget_expand(
            &mut tree,
            BUDGET,
            TOP_K_EXTENSIONS,
            trunc,
            ROOT_TRUNCATION_NOTICE_FALLBACK,
        );
        assert!(
            body.contains("one.py"),
            "small sibling file must be shown: {body}"
        );
        assert!(
            body.contains("- images/"),
            "fat dir must still be listed (summarized): {body}"
        );
        assert!(
            body.contains("[80 files in subtree:") || body.contains("*.jpg"),
            "fat dir should be summarized, not fully listed: {body}"
        );
        let img_lines = body.matches("img_").count();
        assert!(
            img_lines < 10,
            "should not enumerate most images (found {img_lines}): {body}"
        );
        assert!(
            body.len() <= BUDGET,
            "output ({}) must not exceed budget ({BUDGET}): {body}",
            body.len()
        );
    }
    #[tokio::test]
    async fn tool_lists_directory_via_resources() {
        let tmp = TempDir::new().unwrap();
        let subdir = tmp.path().join("src");
        fs::create_dir(&subdir).unwrap();
        File::create(subdir.join("main.rs")).unwrap();
        File::create(subdir.join("lib.rs")).unwrap();
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let tool = ListDirTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            ListDirInput {
                target_directory: ".".to_string(),
            },
        )
        .await
        .unwrap();
        match &output {
            ListDirOutput::Content(c) => {
                assert!(c.content.contains("src/"), "should list src dir");
                assert!(c.content.contains("main.rs"), "should list main.rs");
                assert!(c.content.contains("lib.rs"), "should list lib.rs");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
    #[tokio::test]
    async fn legacy_nonexistent_dir_returns_exact_historical_message() {
        let tmp = TempDir::new().unwrap();
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let tool = ListDirTool;
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            ctx,
            ListDirInput {
                target_directory: "nonexistent".to_string(),
            },
        )
        .await
        .unwrap();
        let expected = format!(
            "Error: {} is not a valid directory",
            tmp.path().join("nonexistent").display()
        );
        match output {
            ListDirOutput::Error(msg) => {
                assert_eq!(msg, expected);
            }
            other => panic!("expected legacy Error for nonexistent dir, got: {other:?}"),
        }
    }
    #[tokio::test]
    async fn legacy_file_path_returns_exact_historical_message() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a_file.txt"), "content\n").unwrap();
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let tool = ListDirTool;
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            ctx,
            ListDirInput {
                target_directory: "a_file.txt".to_string(),
            },
        )
        .await
        .unwrap();
        let expected = format!(
            "Error: {} is not a valid directory",
            tmp.path().join("a_file.txt").display()
        );
        match output {
            ListDirOutput::Error(msg) => {
                assert_eq!(msg, expected);
            }
            other => panic!("expected legacy Error for file path, got: {other:?}"),
        }
    }
    #[tokio::test]
    async fn current_nonexistent_dir_returns_structured_not_found() {
        let tmp = TempDir::new().unwrap();
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let tool = ListDirTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            ListDirInput {
                target_directory: "nonexistent".to_string(),
            },
        )
        .await
        .unwrap();
        match output {
            ListDirOutput::NotFound(msg) => {
                assert!(msg.contains("does not exist"), "got: {msg}");
            }
            other => panic!("expected NotFound for nonexistent dir, got: {other:?}"),
        }
    }
    #[tokio::test]
    async fn current_file_path_returns_structured_is_a_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a_file.txt"), "content\n").unwrap();
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let tool = ListDirTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            ListDirInput {
                target_directory: "a_file.txt".to_string(),
            },
        )
        .await
        .unwrap();
        match output {
            ListDirOutput::IsAFile(msg) => {
                assert!(msg.contains("is a file, not a directory"), "got: {msg}");
            }
            other => panic!("expected IsAFile for file path, got: {other:?}"),
        }
    }
    #[tokio::test]
    async fn tool_uses_params_for_max_output_chars() {
        let tmp = TempDir::new().unwrap();
        for i in 0..30 {
            let subdir = tmp.path().join(format!("dir_{:02}", i));
            fs::create_dir(&subdir).unwrap();
            for j in 0..10 {
                File::create(subdir.join(format!("file_{}.rs", j))).unwrap();
            }
        }
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(Params(ListDirParams {
            max_output_chars: Some(200),
        }));
        let tool = ListDirTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            ListDirInput {
                target_directory: ".".to_string(),
            },
        )
        .await
        .unwrap();
        match output {
            ListDirOutput::Content(c) => {
                assert!(
                    !c.content.contains("file_0.rs"),
                    "individual files should be hidden when fallback triggers"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
    #[tokio::test]
    async fn tool_works_through_erased_interface() {
        let tmp = TempDir::new().unwrap();
        File::create(tmp.path().join("hello.txt")).unwrap();
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let tool = ListDirTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            ListDirInput {
                target_directory: ".".to_string(),
            },
        )
        .await
        .unwrap();
        match output {
            ListDirOutput::Content(c) => {
                assert!(c.content.contains("hello.txt"));
            }
            other => panic!("Expected Content output, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn tool_absolute_path_ignores_cwd() {
        let tmp = TempDir::new().unwrap();
        let subdir = tmp.path().join("abs_test");
        fs::create_dir(&subdir).unwrap();
        File::create(subdir.join("file.rs")).unwrap();
        let mut resources = Resources::new();
        resources.insert(Cwd(PathBuf::from("/does/not/matter")));
        let tool = ListDirTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            ListDirInput {
                target_directory: subdir.to_string_lossy().into_owned(),
            },
        )
        .await
        .unwrap();
        match output {
            ListDirOutput::Content(c) => {
                assert!(c.content.contains("file.rs"));
                assert_eq!(c.absolute_root_path, subdir);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn current_run_matches_direct_budget_expand() {
        const BUDGET: usize = 200;
        let tmp = TempDir::new().unwrap();
        let listed = tmp.path().join("proj");
        fs::create_dir(&listed).unwrap();
        init_minimal_git_worktree(&listed);
        File::create(listed.join("README.md")).unwrap();
        File::create(listed.join("aaa_ignored.log")).unwrap();
        fs::write(listed.join(".gitignore"), "*.log\n").unwrap();
        let src = listed.join("src");
        fs::create_dir(&src).unwrap();
        File::create(src.join("main.rs")).unwrap();
        for i in 0..20 {
            let subdir = listed.join(format!("dir_{i:02}"));
            fs::create_dir(&subdir).unwrap();
            for j in 0..8 {
                File::create(subdir.join(format!("file_{j}.rs"))).unwrap();
            }
        }
        let tools: HashMap<ToolKind, String> = [
            (ToolKind::List, "my_ls".to_string()),
            (ToolKind::Search, "my_grep".to_string()),
            (ToolKind::Execute, "my_bash".to_string()),
        ]
        .into_iter()
        .collect();
        let renderer = TemplateRenderer::new(tools, HashMap::new());
        let notice = root_truncation_notice(Some(&renderer));
        let (mut tree_on, trunc_on) = build_tree(&listed, true);
        let body_on = budget_expand(&mut tree_on, BUDGET, TOP_K_EXTENSIONS, trunc_on, &notice);
        assert!(
            !body_on.contains("aaa_ignored.log"),
            "fixture must hide gitignored name under respect_gitignore=true: {body_on}"
        );
        let (mut tree, trunc) = build_tree(&listed, false);
        let direct = budget_expand(&mut tree, BUDGET, TOP_K_EXTENSIONS, trunc, &notice);
        let expected = format!("- {}/\n{}", listed.display(), direct.trim_end());
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(RespectGitignore(false));
        resources.insert(Params(ListDirParams {
            max_output_chars: Some(BUDGET),
        }));
        resources.insert(renderer);
        let output = echo_agent_tool_runtime::Tool::run(
            &ListDirTool,
            test_ctx(resources.into_shared()),
            ListDirInput {
                target_directory: "proj".to_string(),
            },
        )
        .await
        .unwrap();
        match output {
            ListDirOutput::Content(c) => {
                assert_eq!(c.content, expected);
                assert_eq!(c.absolute_root_path, listed);
                assert!(
                    c.content.contains("my_ls"),
                    "rendered truncation notice must flow through spawn_blocking: {}",
                    c.content
                );
                assert!(
                    c.content.contains("aaa_ignored.log"),
                    "RespectGitignore(false) must surface gitignored names: {}",
                    c.content
                );
                assert!(
                    c.content.contains("too large to list fully"),
                    "tight budget must truncate: {}",
                    c.content
                );
            }
            other => panic!("expected Content, got: {other:?}"),
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn legacy_run_matches_direct_render_legacy() {
        let tmp = TempDir::new().unwrap();
        let listed = tmp.path().join("proj");
        let src = listed.join("src");
        fs::create_dir_all(src.join("util")).unwrap();
        File::create(src.join("main.rs")).unwrap();
        File::create(src.join("lib.rs")).unwrap();
        File::create(src.join("util").join("helpers.rs")).unwrap();
        let tests_dir = listed.join("tests");
        fs::create_dir(&tests_dir).unwrap();
        File::create(tests_dir.join("test_main.rs")).unwrap();
        File::create(listed.join("README.md")).unwrap();
        File::create(listed.join("Cargo.toml")).unwrap();
        let direct =
            versions::legacy_0_4_10::render_legacy(&listed, crate::DEFAULT_TOOL_OUTPUT_BYTES);
        let expected = format!("- {}/\n{}", listed.display(), direct.trim_end());
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let output = echo_agent_tool_runtime::Tool::run(
            &ListDirTool,
            ctx,
            ListDirInput {
                target_directory: "proj".to_string(),
            },
        )
        .await
        .unwrap();
        match output {
            ListDirOutput::Content(c) => {
                assert_eq!(c.content, expected);
                assert_eq!(c.absolute_root_path, listed);
            }
            other => panic!("expected Content, got: {other:?}"),
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn legacy_run_matches_direct_render_legacy_with_byte_budget() {
        const BUDGET: usize = 80;
        let tmp = TempDir::new().unwrap();
        let listed = tmp.path().join("proj");
        fs::create_dir(&listed).unwrap();
        File::create(listed.join("README.md")).unwrap();
        for name in ["aaa", "bbb", "ccc"] {
            let dir = listed.join(name);
            fs::create_dir(&dir).unwrap();
            for i in 0..10 {
                File::create(dir.join(format!("f{i}.rs"))).unwrap();
            }
        }
        let direct = versions::legacy_0_4_10::render_legacy(&listed, BUDGET);
        assert!(
            direct.contains("listing exceeds size limit"),
            "fixture must trip legacy byte fallback: {direct}"
        );
        let expected = format!("- {}/\n{}", listed.display(), direct.trim_end());
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(Params(ListDirParams {
            max_output_chars: Some(BUDGET),
        }));
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let output = echo_agent_tool_runtime::Tool::run(
            &ListDirTool,
            ctx,
            ListDirInput {
                target_directory: "proj".to_string(),
            },
        )
        .await
        .unwrap();
        match output {
            ListDirOutput::Content(c) => {
                assert_eq!(c.content, expected);
                assert!(
                    c.content.contains("listing exceeds size limit"),
                    "Params.max_output_chars must reach render_legacy: {}",
                    c.content
                );
            }
            other => panic!("expected Content, got: {other:?}"),
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn legacy_run_empty_dir_adds_no_children_found() {
        let tmp = TempDir::new().unwrap();
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let output = echo_agent_tool_runtime::Tool::run(
            &ListDirTool,
            ctx,
            ListDirInput {
                target_directory: ".".to_string(),
            },
        )
        .await
        .unwrap();
        let expected = format!("- {}/\n  no children found", tmp.path().display());
        match output {
            ListDirOutput::Content(c) => assert_eq!(c.content, expected),
            other => panic!("expected Content, got: {other:?}"),
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_list_dir_walk_panic_is_tool_error() {
        let listed = Path::new("/tmp/listed");
        let err = spawn_list_dir_walk(listed, || -> String {
            panic!("list_dir walk test panic");
        })
        .await
        .expect_err("walk panic must surface as Err, not abort the runtime");
        assert_eq!(err.kind, echo_agent_tool_runtime::ToolErrorKind::Execution);
        assert_eq!(
            err.detail,
            "Directory listing failed unexpectedly for /tmp/listed; retry or narrow the target directory."
        );
        assert!(
            !err.detail.contains("list_dir walk test panic"),
            "must not leak panic payload into model-visible text: {}",
            err.detail
        );
        let ok = spawn_list_dir_walk(listed, || "ok".to_string())
            .await
            .unwrap();
        assert_eq!(ok, "ok");
    }
    #[tokio::test(flavor = "current_thread")]
    async fn list_dir_join_error_cancel_is_cancelled() {
        let handle = tokio::spawn(std::future::pending::<()>());
        handle.abort();
        let join_err = handle.await.expect_err("aborted task");
        assert!(join_err.is_cancelled());
        let err = map_list_dir_join_error(join_err, Path::new("/tmp/listed"));
        assert_eq!(err.kind, echo_agent_tool_runtime::ToolErrorKind::Cancelled);
        assert_eq!(
            err.detail,
            "Directory listing was cancelled for /tmp/listed."
        );
    }
    #[test]
    fn tool_name_and_description() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = ListDirTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "list_dir"
        );
        assert!(ToolMetadata::description_template(&tool).contains("Lists files and directories"));
        assert!(ToolMetadata::description_template(&tool).contains(".gitignore"));
        assert!(
            ToolMetadata::description_template(&tool)
                .contains("${{ params.list.target_directory }}"),
            "param name should use MiniJinja template, not be hardcoded"
        );
    }
}
