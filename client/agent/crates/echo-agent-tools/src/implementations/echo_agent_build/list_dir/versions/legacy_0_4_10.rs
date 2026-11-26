//! Legacy (0.4.10) depth-threshold directory rendering.
//!
//! Extracted from an earlier revision of the codebase. The current renderer uses
//! BFS character-budget expansion; this module preserves the old depth-based
//! summarization algorithm for `contract_version = "legacy-0.4.10"`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ───────────────────────────────────────────────────────────────────────────
// Configuration
// ───────────────────────────────────────────────────────────────────────────

const ROOT_SUMMARIZATION_THRESHOLD: usize = 1500;
const SUBDIR_SUMMARIZATION_THRESHOLD: usize = 15;
const TOP_K_EXTENSIONS_TO_RENDER: usize = 3;
const DEFAULT_MAX_OUTPUT_BYTES: usize = crate::DEFAULT_TOOL_OUTPUT_BYTES;

#[derive(Debug, Clone)]
struct RenderConfig {
    summarization_threshold_by_depth: Vec<usize>,
    top_k_extensions_to_render: usize,
    max_output_bytes: usize,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            summarization_threshold_by_depth: vec![
                ROOT_SUMMARIZATION_THRESHOLD,
                SUBDIR_SUMMARIZATION_THRESHOLD,
            ],
            top_k_extensions_to_render: TOP_K_EXTENSIONS_TO_RENDER,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl RenderConfig {
    fn with_sub_dirs_always_summarized() -> Self {
        RenderConfig {
            summarization_threshold_by_depth: vec![ROOT_SUMMARIZATION_THRESHOLD],
            top_k_extensions_to_render: TOP_K_EXTENSIONS_TO_RENDER,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Accumulator + helpers
// ───────────────────────────────────────────────────────────────────────────

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

fn filename(path: &Path) -> String {
    path.file_name()
        .map_or("<unknown_filename>".to_string(), |s| {
            s.to_string_lossy().into_owned()
        })
}

// ───────────────────────────────────────────────────────────────────────────
// Tree structures
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct ChildEntry {
    is_dir: bool,
    name: String,
    path: PathBuf,
}

#[derive(Debug, Default)]
struct DirectoryView {
    depth: usize,
    child_count: usize,
    children: Option<Vec<ChildEntry>>,
    subtree: DirAccum,
}

#[derive(Debug)]
struct Collected {
    dirs: HashMap<PathBuf, DirectoryView>,
}

fn should_summarize(depth: usize, child_count: usize, cfg: &RenderConfig) -> bool {
    cfg.summarization_threshold_by_depth
        .get(depth)
        .is_none_or(|threshold| child_count >= *threshold)
}

fn get_or_init_dir<'a>(
    dirs: &'a mut HashMap<PathBuf, DirectoryView>,
    path: &Path,
    depth: usize,
    cfg: &RenderConfig,
) -> &'a mut DirectoryView {
    dirs.entry(path.to_path_buf())
        .or_insert_with(|| DirectoryView {
            depth,
            child_count: 0,
            children: if depth < cfg.summarization_threshold_by_depth.len() {
                Some(Vec::new())
            } else {
                None
            },
            subtree: DirAccum::default(),
        })
}

// ───────────────────────────────────────────────────────────────────────────
// Collection + rendering
// ───────────────────────────────────────────────────────────────────────────

fn collect(root_path: &Path, walker: ignore::Walk, cfg: &RenderConfig) -> Collected {
    let mut dirs: HashMap<PathBuf, DirectoryView> = HashMap::new();
    for directory_entry in walker {
        let Ok(entry) = directory_entry else { continue };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let depth = entry.depth();

        if file_type.is_dir() && depth <= cfg.summarization_threshold_by_depth.len() {
            get_or_init_dir(&mut dirs, path, depth, cfg);
        }
        if depth == 0 {
            continue;
        }
        let Some(parent_path) = path.parent() else {
            continue;
        };
        let parent_depth = depth.saturating_sub(1);
        if parent_depth <= cfg.summarization_threshold_by_depth.len() {
            let parent_view = get_or_init_dir(&mut dirs, parent_path, parent_depth, cfg);
            parent_view.child_count += 1;
            if let Some(children) = parent_view.children.as_mut() {
                let name = filename(path);
                children.push(ChildEntry {
                    is_dir: file_type.is_dir(),
                    name,
                    path: path.to_path_buf(),
                });
                if should_summarize(parent_view.depth, parent_view.child_count, cfg) {
                    parent_view.children = None;
                }
            }
        }
        if file_type.is_file() {
            let ext_key = ext_key_from_path(path);
            let mut ancestors: Vec<&Path> = path
                .ancestors()
                .take_while(|p| p.starts_with(root_path))
                .collect();
            ancestors.pop();
            ancestors.reverse();
            for (depth, p) in ancestors
                .into_iter()
                .take(cfg.summarization_threshold_by_depth.len())
                .enumerate()
            {
                let view = get_or_init_dir(&mut dirs, p, depth, cfg);
                view.subtree.add_ext(&ext_key);
            }
        }
    }
    for view in dirs.values_mut() {
        if let Some(children) = view.children.as_mut() {
            children.sort_by(|a, b| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            });
        }
    }
    Collected { dirs }
}

fn render(root: &Path, collected: &Collected, cfg: &RenderConfig) -> Vec<String> {
    fn render_dir(
        lines: &mut Vec<String>,
        dir_path: &Path,
        collected: &Collected,
        cfg: &RenderConfig,
    ) {
        let view = match collected.dirs.get(dir_path) {
            Some(v) => v,
            None => return,
        };
        let indent = "  ".repeat(view.depth);
        lines.push(format!("{}- {}/", indent, filename(dir_path)));
        if should_summarize(view.depth, view.child_count, cfg) {
            let summary = view.subtree.to_summary(cfg.top_k_extensions_to_render);
            if summary.is_empty() {
                return;
            }
            let indent_child = "  ".repeat(view.depth + 1);
            lines.push(format!("{}{}", indent_child, summary));
        } else if let Some(children) = &view.children {
            for child in children {
                if child.is_dir {
                    render_dir(lines, &child.path, collected, cfg);
                } else {
                    let indent_child = "  ".repeat(view.depth + 1);
                    lines.push(format!("{}- {}", indent_child, child.name));
                }
            }
        }
    }
    let mut lines: Vec<String> = Vec::new();
    render_dir(&mut lines, root, collected, cfg);
    lines
}

fn output_byte_count(lines: &[String]) -> usize {
    if lines.is_empty() {
        return 0;
    }
    lines.iter().map(|l| l.len()).sum::<usize>() + lines.len() - 1
}

fn render_with_fallback(root: &Path, collected: &Collected, cfg: &RenderConfig) -> Vec<String> {
    let lines = render(root, collected, cfg);
    let byte_count = output_byte_count(&lines);
    if byte_count > cfg.max_output_bytes {
        let mut lines = render(
            root,
            collected,
            &RenderConfig::with_sub_dirs_always_summarized(),
        );
        tracing::debug!(
            path = %root.display(),
            original_bytes = byte_count,
            limit = cfg.max_output_bytes,
            "legacy list_dir output exceeded size limit, falling back to shallow rendering"
        );
        lines.push(
            "[Subdirectories not expanded: listing exceeds size limit. \
             Use list_dir on a specific subdirectory to see its contents.]"
                .to_string(),
        );
        lines
    } else {
        lines
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Public entry point
// ───────────────────────────────────────────────────────────────────────────

/// Render a directory listing using the legacy (0.4.10) depth-threshold algorithm.
///
/// Returns the body text (without the root path header line).
pub(crate) fn render_legacy(root: &Path, max_output_bytes: usize) -> String {
    let cfg = RenderConfig {
        max_output_bytes,
        ..Default::default()
    };
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(true)
        .build();
    let collected = collect(root, walker, &cfg);
    let output_lines = render_with_fallback(root, &collected, &cfg);
    // Skip the first line (root dir name) — the caller prepends its own.
    if output_lines.len() > 1 {
        output_lines[1..].join("\n")
    } else {
        String::new()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests — fixture-based historical verification
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a reference directory tree for fixture comparison.
    fn create_fixture_tree(root: &std::path::Path) {
        // src/
        //   main.rs
        //   lib.rs
        //   util/
        //     helpers.rs
        // tests/
        //   test_main.rs
        // README.md
        // Cargo.toml
        let src = root.join("src");
        std::fs::create_dir_all(src.join("util")).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("lib.rs"), "pub mod util;").unwrap();
        std::fs::write(src.join("util").join("helpers.rs"), "pub fn help() {}").unwrap();

        let tests = root.join("tests");
        std::fs::create_dir_all(&tests).unwrap();
        std::fs::write(tests.join("test_main.rs"), "#[test] fn it_works() {}").unwrap();

        std::fs::write(root.join("README.md"), "# Project").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
    }

    /// The legacy depth-based renderer expands small directories fully,
    /// showing individual files with indentation per depth level.
    /// Archived-output fixture test: asserts exact string equality against
    /// the known output of the earlier depth-threshold algorithm.
    ///
    /// Fixture captured from `render_legacy()` on the reference tree defined
    /// by `create_fixture_tree()`. If this test fails after a change to the
    /// legacy renderer, the change has drifted from historical behavior.
    #[test]
    fn legacy_renders_small_tree_exact_fixture() {
        let tmp = TempDir::new().unwrap();
        create_fixture_tree(tmp.path());
        let body = render_legacy(tmp.path(), 40_000);

        // Exact archived output from the depth-threshold algorithm.
        // Root files listed alphabetically, src/ expanded (< 15 children),
        // util/ summarized (depth >= 2), tests/ expanded.
        let expected = "  - Cargo.toml\n  - README.md\n  - src/\n    - lib.rs\n    - main.rs\n    - util/\n      [1 file in subtree: 1 *.rs]\n  - tests/\n    - test_main.rs";

        assert_eq!(
            body, expected,
            "legacy renderer output must match archived fixture exactly.\n\
             Got:\n{body}\n\nExpected:\n{expected}"
        );
    }

    /// Empty directories should produce empty output (no "no children found"
    /// — that is added by the caller in mod.rs, not by the renderer).
    #[test]
    fn legacy_empty_directory_returns_empty_string() {
        let tmp = TempDir::new().unwrap();
        let body = render_legacy(tmp.path(), 40_000);
        assert!(
            body.is_empty(),
            "empty dir should produce empty body, got: {body}"
        );
    }

    /// The depth-based algorithm summarizes directories when child count
    /// exceeds the threshold (15 for subdirs by default). Verify that a
    /// large directory gets a summary line instead of full expansion.
    #[test]
    fn legacy_summarizes_large_subdirectory() {
        let tmp = TempDir::new().unwrap();
        let large_dir = tmp.path().join("many_files");
        std::fs::create_dir_all(&large_dir).unwrap();
        // Create 20 files to exceed the SUBDIR_SUMMARIZATION_THRESHOLD (15).
        for i in 0..20 {
            std::fs::write(large_dir.join(format!("file_{i}.rs")), "").unwrap();
        }
        let body = render_legacy(tmp.path(), 40_000);

        // Should show a summary line with file count and extension breakdown.
        assert!(
            body.contains("files in subtree") || body.contains("file in subtree"),
            "large dir should be summarized: {body}"
        );
        assert!(
            body.contains("*.rs"),
            "summary should mention .rs extension: {body}"
        );
    }

    /// Verify structural equivalence: the depth-based renderer produces
    /// lines with consistent 2-space indentation matching the historical
    /// algorithm's output pattern.
    #[test]
    fn legacy_indentation_matches_historical_pattern() {
        let tmp = TempDir::new().unwrap();
        create_fixture_tree(tmp.path());
        let body = render_legacy(tmp.path(), 40_000);

        // Every line should start with some number of "  " pairs followed by "- "
        // or be a summary line (starts with spaces + "[").
        for line in body.lines() {
            let trimmed = line.trim_start();
            let indent_chars = line.len() - trimmed.len();
            assert!(
                indent_chars % 2 == 0,
                "indentation must be even (2-space per level), got {} in: {line}",
                indent_chars
            );
            assert!(
                trimmed.starts_with("- ") || trimmed.starts_with('['),
                "line must start with '- ' or '[': {line}"
            );
        }
    }
}
