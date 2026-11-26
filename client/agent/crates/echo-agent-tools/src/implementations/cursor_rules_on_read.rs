//! Cursor project-rule reminders attached after successful file reads.

use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use serde::{Deserialize, Serialize};

use crate::types::resources::{SharedResources, State};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CursorRulesOnReadTracker {
    #[serde(default)]
    scanned_scope_dirs: HashSet<PathBuf>,
    #[serde(default)]
    rules: Vec<ParsedCursorRule>,
    #[serde(default)]
    injected_rule_paths: HashSet<PathBuf>,
}

crate::register_resource!("cursor", "RulesOnRead", CursorRulesOnReadTracker);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParsedCursorRule {
    full_path: PathBuf,
    scope_dir: PathBuf,
    body: String,
    kind: CursorRuleKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum CursorRuleKind {
    Global,
    FileGlobbed(Vec<String>),
    AgentFetched,
    Manual,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CursorRuleFrontmatter {
    #[serde(default)]
    always_apply: bool,
    #[serde(default)]
    globs: Option<GlobField>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum GlobField {
    String(String),
    List(Vec<String>),
}

impl GlobField {
    fn into_patterns(self) -> Vec<String> {
        match self {
            Self::String(value) => value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            Self::List(values) => values
                .into_iter()
                .flat_map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .collect(),
        }
    }
}

pub async fn append_cursor_rules_for_read(
    enabled: bool,
    resources: SharedResources,
    workspace_root: &Path,
    read_path: &Path,
    content: &mut String,
    content_concise: &mut Option<String>,
) {
    if !enabled {
        return;
    }
    let Some(reminder) =
        cursor_rule_reminder_for_read_inner(resources, workspace_root, read_path).await
    else {
        return;
    };
    append_reminder(content, &reminder);
    if let Some(concise) = content_concise {
        append_reminder(concise, &reminder);
    }
}

fn append_reminder(content: &mut String, reminder: &str) {
    if !content.is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(reminder);
}

async fn cursor_rule_reminder_for_read_inner(
    resources: SharedResources,
    workspace_root: &Path,
    read_path: &Path,
) -> Option<String> {
    let workspace_root = normalize_existing(workspace_root).await;
    let read_path = normalize_existing(read_path).await;
    let scope_dirs = ancestor_scope_dirs(&workspace_root, &read_path);
    if scope_dirs.is_empty() {
        return None;
    }

    let (dirs_to_scan, injected_rule_paths_to_normalize) = {
        let mut res = resources.lock().await;
        let tracker = &mut res.get_or_default::<State<CursorRulesOnReadTracker>>().0;
        let dirs_to_scan = scope_dirs
            .iter()
            .filter(|dir| !tracker.scanned_scope_dirs.contains(*dir))
            .cloned()
            .collect::<Vec<_>>();
        (
            dirs_to_scan,
            tracker
                .injected_rule_paths
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        )
    };

    let mut discovered = Vec::new();
    for scope_dir in &dirs_to_scan {
        discovered.extend(scan_scope_dir(scope_dir).await);
    }
    let normalized_injected_rule_paths =
        normalize_existing_paths(injected_rule_paths_to_normalize, &workspace_root).await;

    let matching_rules = {
        let mut res = resources.lock().await;
        let tracker = &mut res.get_or_default::<State<CursorRulesOnReadTracker>>().0;
        tracker
            .injected_rule_paths
            .extend(normalized_injected_rule_paths);
        tracker.scanned_scope_dirs.extend(dirs_to_scan);
        append_new_rules(&mut tracker.rules, discovered);

        let mut matching_rules = Vec::new();
        for rule in &tracker.rules {
            if tracker.injected_rule_paths.contains(&rule.full_path) {
                continue;
            }
            if rule_matches_read_path(rule, &workspace_root, &read_path) {
                tracker.injected_rule_paths.insert(rule.full_path.clone());
                matching_rules.push(rule.clone());
            }
        }
        matching_rules
    };

    render_cursor_rule_reminder(&matching_rules)
}

fn append_new_rules(existing: &mut Vec<ParsedCursorRule>, discovered: Vec<ParsedCursorRule>) {
    let mut by_path = existing
        .iter()
        .enumerate()
        .map(|(idx, rule)| (rule.full_path.clone(), idx))
        .collect::<std::collections::HashMap<_, _>>();

    for rule in discovered {
        match by_path.entry(rule.full_path.clone()) {
            Entry::Occupied(_) => {}
            Entry::Vacant(entry) => {
                entry.insert(existing.len());
                existing.push(rule);
            }
        }
    }
}

fn ancestor_scope_dirs(workspace_root: &Path, read_path: &Path) -> Vec<PathBuf> {
    let Some(parent) = read_path.parent() else {
        return Vec::new();
    };
    if !parent.starts_with(workspace_root) {
        return Vec::new();
    }

    let mut dirs = Vec::new();
    let mut current = Some(parent);
    while let Some(dir) = current {
        if !dir.starts_with(workspace_root) {
            break;
        }
        dirs.push(dir.to_path_buf());
        if dir == workspace_root {
            break;
        }
        current = dir.parent();
    }
    dirs.reverse();
    dirs
}

async fn normalize_existing(path: &Path) -> PathBuf {
    crate::util::fs::canonicalize_with_timeout(path.to_path_buf()).await
}

async fn normalize_existing_paths(
    paths: impl IntoIterator<Item = PathBuf>,
    workspace_root: &Path,
) -> HashSet<PathBuf> {
    let mut normalized = HashSet::new();
    for path in paths {
        let absolute_path = if path.is_relative() {
            workspace_root.join(&path)
        } else {
            path
        };
        normalized.insert(normalize_existing(&absolute_path).await);
    }
    normalized
}

async fn scan_scope_dir(scope_dir: &Path) -> Vec<ParsedCursorRule> {
    let rules_dir = scope_dir.join(".cursor").join("rules");
    let Ok(metadata) = tokio::fs::metadata(&rules_dir).await else {
        return Vec::new();
    };
    if !metadata.is_dir() {
        return Vec::new();
    }

    let mut rules = Vec::new();
    let mut stack = vec![rules_dir];
    while let Some(dir) = stack.pop() {
        let Ok(mut read_dir) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            match entry.file_type().await {
                Ok(file_type) if file_type.is_dir() => dirs.push(path),
                Ok(file_type) if file_type.is_file() && is_rule_file(&path) => files.push(path),
                _ => {}
            }
        }
        dirs.sort();
        files.sort();
        stack.extend(dirs.into_iter().rev());
        for file in files {
            let Ok(content) = tokio::fs::read_to_string(&file).await else {
                continue;
            };
            let full_path = normalize_existing(&file).await;
            rules.push(parse_rule(scope_dir.to_path_buf(), full_path, &content));
        }
    }
    rules
}

fn is_rule_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mdc") || ext.eq_ignore_ascii_case("md"))
}

fn parse_rule(scope_dir: PathBuf, full_path: PathBuf, content: &str) -> ParsedCursorRule {
    let (frontmatter, body) = split_frontmatter(content)
        .map(|(frontmatter, body)| (Some(frontmatter), body.to_owned()))
        .unwrap_or((None, content.to_owned()));

    let parsed = frontmatter.and_then(parse_frontmatter);
    let kind = match parsed {
        Some(fm) if fm.always_apply => CursorRuleKind::Global,
        Some(fm) => {
            let globs = fm.globs.map(GlobField::into_patterns).unwrap_or_default();
            if !globs.is_empty() {
                CursorRuleKind::FileGlobbed(globs)
            } else if fm
                .description
                .is_some_and(|description| !description.trim().is_empty())
            {
                CursorRuleKind::AgentFetched
            } else {
                CursorRuleKind::Manual
            }
        }
        None => CursorRuleKind::Manual,
    };

    ParsedCursorRule {
        full_path,
        scope_dir,
        body: body.trim_start().to_owned(),
        kind,
    }
}

fn parse_frontmatter(yaml: &str) -> Option<CursorRuleFrontmatter> {
    if let Ok(parsed) = serde_yaml::from_str::<CursorRuleFrontmatter>(yaml) {
        return Some(parsed);
    }

    let mut parsed = CursorRuleFrontmatter::default();
    let mut saw_field = false;
    for line in yaml.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key {
            "alwaysApply" => {
                parsed.always_apply = value.eq_ignore_ascii_case("true");
                saw_field = true;
            }
            "globs" if !value.is_empty() => {
                parsed.globs = Some(GlobField::String(value.to_owned()));
                saw_field = true;
            }
            "description" if !value.is_empty() => {
                parsed.description = Some(value.to_owned());
                saw_field = true;
            }
            _ => {}
        }
    }

    saw_field.then_some(parsed)
}

fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;

    for delimiter in ["\n---\n", "\n---\r\n", "\r\n---\r\n", "\r\n---\n"] {
        if let Some(index) = rest.find(delimiter) {
            let body_start = index + delimiter.len();
            return Some((&rest[..index], &rest[body_start..]));
        }
    }

    if rest.trim_end() == "---" {
        return Some(("", ""));
    }

    None
}

fn rule_matches_read_path(
    rule: &ParsedCursorRule,
    workspace_root: &Path,
    read_path: &Path,
) -> bool {
    match &rule.kind {
        CursorRuleKind::Global => {
            rule.scope_dir != workspace_root && read_path.starts_with(&rule.scope_dir)
        }
        CursorRuleKind::FileGlobbed(globs) => file_globs_match(&rule.scope_dir, read_path, globs),
        CursorRuleKind::AgentFetched | CursorRuleKind::Manual => false,
    }
}

fn file_globs_match(scope_dir: &Path, read_path: &Path, globs: &[String]) -> bool {
    let absolute_read_path = path_to_unix(read_path);
    let relative_candidate = read_path
        .strip_prefix(scope_dir)
        .ok()
        .map(path_to_unix)
        .unwrap_or_default();

    globs.iter().any(|glob| {
        let normalized = glob.replace('\\', "/");
        if Path::new(&normalized).is_absolute() {
            return glob_matches(&normalized, &absolute_read_path);
        }
        glob_matches(&normalized, &relative_candidate)
            || glob_matches(&normalize_relative_glob(&normalized), &relative_candidate)
    })
}

fn normalize_relative_glob(glob: &str) -> String {
    if !glob.contains('/') || glob.ends_with('/') {
        format!("**/{glob}")
    } else {
        glob.to_owned()
    }
}

fn glob_matches(pattern: &str, candidate: &str) -> bool {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .is_ok_and(|glob| glob.compile_matcher().is_match(candidate))
}

fn path_to_unix(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

fn render_cursor_rule_reminder(rules: &[ParsedCursorRule]) -> Option<String> {
    if rules.is_empty() {
        return None;
    }

    let mut lines =
        vec!["The following rule files are relevant to the files you just read:".to_owned()];
    for rule in rules {
        let body = if rule.body.trim().is_empty() {
            "(Rule file is empty.)"
        } else {
            rule.body.trim_end()
        };
        lines.push(format!("- {}\n{body}", rule.full_path.display()));
    }
    lines.push("Consider these rules if they affect your changes.".to_owned());
    Some(lines.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::*;
    use crate::computer::local::LocalFs;
    use crate::implementations::echo_agent_build::read_file::{
        ReadFileInput, ReadFileParams, ReadFileTool as EchoAgentReadFileTool,
    };
    use crate::notification::types::ToolNotificationHandle;
    use crate::types::output::ReadFileOutput;
    use crate::types::resources::NotificationHandle;
    use crate::types::resources::{Cwd, FileSystem, Params, Resources, State};
    use crate::types::template_renderer::TemplateRenderer;
    use crate::types::tool::ToolKind;
    use crate::types::tool_metadata::test_ctx;

    fn resources(root: &Path) -> SharedResources {
        let mut resources = Resources::new();
        resources.insert(Cwd(root.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources.insert(TemplateRenderer::new(
            [(ToolKind::Search, "grep".to_owned())]
                .into_iter()
                .collect(),
            Default::default(),
        ));
        resources.into_shared()
    }

    fn resources_with_echo_agent_rules_on_read(root: &Path) -> SharedResources {
        let mut resources = Resources::new();
        resources.insert(Cwd(root.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources.insert(TemplateRenderer::new(
            [(ToolKind::Search, "grep".to_owned())]
                .into_iter()
                .collect(),
            Default::default(),
        ));
        resources.insert(Params(ReadFileParams {
            cursor_rules_on_read: true,
        }));
        resources.into_shared()
    }

    #[tokio::test]
    async fn file_globbed_rule_matches_read_path_once() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let rules_dir = root.join(".cursor/rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("rust.mdc"),
            "---\nglobs: *.rs\nalwaysApply: false\n---\nUse Rust rules.",
        )
        .unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let shared = resources(root);
        let first =
            cursor_rule_reminder_for_read_inner(shared.clone(), root, &root.join("main.rs")).await;
        assert!(
            first
                .as_deref()
                .is_some_and(|text| text.contains("Use Rust rules."))
        );

        let second = cursor_rule_reminder_for_read_inner(shared, root, &root.join("main.rs")).await;
        assert!(second.is_none(), "rule reminders are deduped per session");
    }

    #[tokio::test]
    async fn seeded_injected_rule_paths_suppress_prefix_duplicates() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let rule_path = root.join(".cursor/rules/rust.mdc");
        std::fs::create_dir_all(rule_path.parent().unwrap()).unwrap();
        std::fs::write(
            &rule_path,
            "---\nglobs: *.rs\nalwaysApply: false\n---\nUse Rust rules.",
        )
        .unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let shared = resources(root);
        {
            let mut res = shared.lock().await;
            let tracker = &mut res.get_or_default::<State<CursorRulesOnReadTracker>>().0;
            tracker
                .injected_rule_paths
                .insert(dunce::canonicalize(&rule_path).unwrap());
        }

        let reminder =
            cursor_rule_reminder_for_read_inner(shared, root, &root.join("main.rs")).await;
        assert!(
            reminder.is_none(),
            "rules injected in the prefix should not be re-emitted"
        );
    }

    #[tokio::test]
    async fn non_canonical_seeded_injected_rule_paths_suppress_prefix_duplicates() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let rule_path = root.join(".cursor/rules/rust.mdc");
        std::fs::create_dir_all(rule_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(root.join("subdir")).unwrap();
        std::fs::write(
            &rule_path,
            "---\nglobs: *.rs\nalwaysApply: false\n---\nUse Rust rules.",
        )
        .unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let shared = resources(root);
        {
            let mut res = shared.lock().await;
            let tracker = &mut res.get_or_default::<State<CursorRulesOnReadTracker>>().0;
            tracker
                .injected_rule_paths
                .insert(root.join("subdir/../.cursor/rules/rust.mdc"));
        }

        let reminder =
            cursor_rule_reminder_for_read_inner(shared, root, &root.join("main.rs")).await;
        assert!(
            reminder.is_none(),
            "non-canonical prefix paths should still dedupe against scanned rule paths"
        );
    }

    #[tokio::test]
    async fn relative_seeded_injected_rule_paths_resolve_against_workspace_root() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let rule_path = root.join(".cursor/rules/rust.mdc");
        std::fs::create_dir_all(rule_path.parent().unwrap()).unwrap();
        std::fs::write(
            &rule_path,
            "---\nglobs: *.rs\nalwaysApply: false\n---\nUse Rust rules.",
        )
        .unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let shared = resources(root);
        {
            let mut res = shared.lock().await;
            let tracker = &mut res.get_or_default::<State<CursorRulesOnReadTracker>>().0;
            tracker
                .injected_rule_paths
                .insert(PathBuf::from(".cursor/rules/rust.mdc"));
        }

        let reminder =
            cursor_rule_reminder_for_read_inner(shared, root, &root.join("main.rs")).await;
        assert!(
            reminder.is_none(),
            "relative prefix paths should resolve from the workspace root before dedupe"
        );
    }

    #[tokio::test]
    async fn root_always_apply_rule_is_not_read_scoped() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let rules_dir = root.join(".cursor/rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("global.mdc"),
            "---\nalwaysApply: true\n---\nGlobal rule.",
        )
        .unwrap();
        std::fs::write(root.join("main.ts"), "const x = 1;\n").unwrap();

        let reminder =
            cursor_rule_reminder_for_read_inner(resources(root), root, &root.join("main.ts")).await;
        assert!(reminder.is_none());
    }

    #[tokio::test]
    async fn nested_always_apply_rule_is_read_scoped() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let frontend = root.join("frontend");
        let rules_dir = frontend.join(".cursor/rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("frontend.mdc"),
            "---\nalwaysApply: true\n---\nFrontend rule.",
        )
        .unwrap();
        std::fs::create_dir_all(frontend.join("src")).unwrap();
        std::fs::write(
            frontend.join("src/App.tsx"),
            "export const App = () => null;\n",
        )
        .unwrap();
        std::fs::write(root.join("server.ts"), "export const server = true;\n").unwrap();

        let shared = resources(root);
        let outside =
            cursor_rule_reminder_for_read_inner(shared.clone(), root, &root.join("server.ts"))
                .await;
        assert!(outside.is_none());

        let inside =
            cursor_rule_reminder_for_read_inner(shared, root, &frontend.join("src/App.tsx")).await;
        assert!(
            inside
                .as_deref()
                .is_some_and(|text| text.contains("Frontend rule."))
        );
    }

    #[tokio::test]
    async fn duplicate_scans_do_not_duplicate_rule_cache() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let rules_dir = root.join(".cursor/rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("rust.mdc"),
            "---\nglobs: *.rs\nalwaysApply: false\n---\nUse Rust rules.",
        )
        .unwrap();

        let first = scan_scope_dir(root).await;
        let second = scan_scope_dir(root).await;
        let mut rules = Vec::new();
        append_new_rules(&mut rules, first);
        append_new_rules(&mut rules, second);

        assert_eq!(rules.len(), 1, "repeated scans should dedupe by rule path");
        assert!(rules[0].body.contains("Use Rust rules."));
    }

    #[tokio::test]
    async fn echo_agent_read_file_output_includes_matching_rule_once() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".cursor/rules")).unwrap();
        std::fs::write(
            root.join(".cursor/rules/rust.mdc"),
            "---\nglobs: *.rs\nalwaysApply: false\n---\nUse Rust rules.",
        )
        .unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let shared = resources_with_echo_agent_rules_on_read(root);
        let input = ReadFileInput {
            document: None,
            path: "main.rs".to_owned(),
            offset: None,
            limit: None,
            pages: None,
            format: None,
        };

        let first =
            echo_agent_tool_runtime::Tool::run(&EchoAgentReadFileTool, test_ctx(shared), input)
                .await
                .unwrap();
        let ReadFileOutput::FileContent(first) = first else {
            panic!("expected file content");
        };
        assert!(
            first
                .content
                .contains("The following rule files are relevant to the files you just read:")
        );
        assert!(first.content.contains("Use Rust rules."));
        assert!(!first.content.contains("cursor rule files"));
        assert!(
            first
                .content_concise
                .as_deref()
                .is_some_and(|content| content.contains("Use Rust rules."))
        );
    }

    #[tokio::test]
    async fn parallel_reads_do_not_lose_matching_rule() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".cursor/rules")).unwrap();
        std::fs::write(
            root.join(".cursor/rules/rust.mdc"),
            "---\nglobs: *.rs\nalwaysApply: false\n---\nUse Rust rules.",
        )
        .unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "# docs\n").unwrap();

        let shared = resources(root);
        let matching_path = root.join("main.rs");
        let non_matching_path = root.join("README.md");
        let matching = cursor_rule_reminder_for_read_inner(shared.clone(), root, &matching_path);
        let non_matching = cursor_rule_reminder_for_read_inner(shared, root, &non_matching_path);
        let (matching, non_matching) = tokio::join!(matching, non_matching);

        assert!(
            matching
                .as_deref()
                .is_some_and(|text| text.contains("Use Rust rules.")),
            "matching parallel read should receive the rule"
        );
        assert!(
            non_matching.is_none(),
            "non-matching parallel read should not receive the rule"
        );
    }

    #[test]
    fn reminder_header_omits_cursor_product_name() {
        let reminder = render_cursor_rule_reminder(&[ParsedCursorRule {
            full_path: PathBuf::from("/repo/.cursor/rules/example.mdc"),
            scope_dir: PathBuf::from("/repo"),
            body: "Rule body.".to_owned(),
            kind: CursorRuleKind::FileGlobbed(vec!["*.rs".to_owned()]),
        }])
        .unwrap();

        assert!(
            reminder
                .starts_with("The following rule files are relevant to the files you just read:")
        );
        assert!(!reminder.contains("cursor rule files"));
    }
}
