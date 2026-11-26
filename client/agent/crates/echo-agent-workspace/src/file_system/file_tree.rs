//! Generates the file-tree based on how we are using it during training
//! This gives the model an overview of the project and helps it navigate and understand
//! the repository better, cold-starting the exploration

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use dashmap::DashMap;
use ignore::{WalkBuilder, WalkState};

use crate::file_system::FsError;

/// Number of threads for parallel directory walking
const NUM_WALK_THREADS: usize = 8;

/// Configuration for limiting file tree traversal to prevent runaway I/O
/// on very large or deeply nested directories.
#[derive(Debug, Clone, Copy)]
pub struct ListContentsLimits {
    /// Maximum number of characters in the output
    pub max_characters: usize,
    /// Maximum depth to traverse (0 = root only)
    pub max_depth: usize,
    /// Maximum number of directories to visit during traversal
    pub max_dirs_visited: usize,
}

impl Default for ListContentsLimits {
    fn default() -> Self {
        Self {
            max_characters: 10_000,
            max_depth: 12,
            max_dirs_visited: 2000,
        }
    }
}

impl ListContentsLimits {
    pub fn new(max_characters: usize, max_depth: usize, max_dirs_visited: usize) -> Self {
        Self {
            max_characters,
            max_depth,
            max_dirs_visited,
        }
    }
}

fn get_top_exts(files: &[String]) -> Vec<(String, usize)> {
    let mut ext_counts: HashMap<String, usize> = HashMap::new();
    for item in files {
        let ext = if let Some(e) = Path::new(item).extension() {
            format!(".{}", e.to_str().unwrap_or("").to_lowercase())
        } else {
            String::new()
        };
        *ext_counts.entry(ext).or_insert(0) += 1;
    }
    let mut vec: Vec<_> = ext_counts.into_iter().collect();
    vec.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    vec
}

fn get_file_ext_str(files: &[String], k: usize) -> String {
    let top_exts = get_top_exts(files);
    let include_dots = top_exts.len() > k
        || (top_exts.len() == k && top_exts.iter().any(|(ext, _)| ext.is_empty()));
    let top_k_exts = &top_exts[0..std::cmp::min(k, top_exts.len())];
    if top_k_exts.is_empty() {
        return String::new();
    }
    if top_k_exts.len() == 1 && top_k_exts[0].0.is_empty() {
        return "(...)".to_string();
    }
    let filtered_top_k_exts: Vec<_> = top_k_exts
        .iter()
        .filter(|(ext, _)| !ext.is_empty())
        .collect();
    let top_counts = filtered_top_k_exts
        .iter()
        .map(|(ext, cnt)| format!("{} *{}", cnt, ext))
        .collect::<Vec<_>>()
        .join(", ");
    if include_dots {
        format!("({top_counts}, ...)")
    } else if top_counts.is_empty() {
        String::new()
    } else {
        format!("({top_counts})")
    }
}

/// Pre-collected directory contents from a single walk
struct DirContents {
    files: Vec<String>,
    dirs: Vec<String>,
}

/// Performs a single parallel walk and collects all directory contents into a map.
/// Returns a map from directory path -> (files, subdirs) in that directory.
fn collect_all_contents(
    root: &Path,
    max_depth: usize,
    max_dirs: usize,
) -> Result<HashMap<PathBuf, DirContents>, FsError> {
    let _timer = (); // instrumentation_timer noop (dev infra)

    let contents_map: DashMap<PathBuf, DirContents> = DashMap::new();
    let files_count = std::sync::atomic::AtomicUsize::new(0);
    let dirs_count = std::sync::atomic::AtomicUsize::new(0);
    let entries_visited = std::sync::atomic::AtomicUsize::new(0);

    // Initialize root entry
    contents_map.insert(
        root.to_path_buf(),
        DirContents {
            files: Vec::new(),
            dirs: Vec::new(),
        },
    );

    let walker = WalkBuilder::new(root)
        .max_depth(Some(max_depth + 1)) // +1 because depth 0 is root itself
        .follow_links(false)
        .same_file_system(true)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .hidden(true)
        .threads(NUM_WALK_THREADS)
        .build_parallel();

    tracing::debug!(
        root = %root.display(),
        max_depth = max_depth,
        max_dirs = max_dirs,
        threads = NUM_WALK_THREADS,
        "Starting parallel file walk"
    );

    {
        let _timer = (); // instrumentation_timer noop (dev infra)
        walker.run(|| {
            let contents_map = &contents_map;
            let files_count = &files_count;
            let dirs_count = &dirs_count;
            let entries_visited = &entries_visited;
            Box::new(move |entry| {
                entries_visited.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // Stop early if we've collected enough directories
                if dirs_count.load(std::sync::atomic::Ordering::Relaxed) >= max_dirs {
                    return WalkState::Quit;
                }

                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => return WalkState::Continue,
                };

                // Skip root itself
                if entry.depth() == 0 {
                    return WalkState::Continue;
                }

                let Some(file_type) = entry.file_type() else {
                    return WalkState::Continue;
                };

                let path = entry.path();
                let Some(parent) = path.parent() else {
                    return WalkState::Continue;
                };
                let name = entry.file_name().to_string_lossy().to_string();

                if file_type.is_dir() {
                    dirs_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    contents_map
                        .entry(parent.to_path_buf())
                        .or_insert_with(|| DirContents {
                            files: Vec::new(),
                            dirs: Vec::new(),
                        })
                        .dirs
                        .push(format!("{name}/"));
                    // Pre-create entry for this directory (even if empty)
                    contents_map
                        .entry(path.to_path_buf())
                        .or_insert_with(|| DirContents {
                            files: Vec::new(),
                            dirs: Vec::new(),
                        });
                } else if file_type.is_file() {
                    files_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    contents_map
                        .entry(parent.to_path_buf())
                        .or_insert_with(|| DirContents {
                            files: Vec::new(),
                            dirs: Vec::new(),
                        })
                        .files
                        .push(name);
                }

                WalkState::Continue
            })
        });
    }

    let _entries_total = entries_visited.load(std::sync::atomic::Ordering::Relaxed);
    // (instrumentation_timer.with_field calls removed — dev-only, not part of Phase 1 move)

    let mut contents_map: HashMap<PathBuf, DirContents> = {
        let _timer = (); // instrumentation_timer noop (dev infra)
        contents_map.into_iter().collect()
    };

    // Sort all entries for stable output
    {
        let _timer = (); // instrumentation_timer noop (dev infra)
        for contents in contents_map.values_mut() {
            contents.files.sort_by_cached_key(|n| n.to_lowercase());
            contents.dirs.sort_by_cached_key(|n| n.to_lowercase());
        }
    }

    Ok(contents_map)
}

struct DirectoryNode {
    depth: usize,
    path: PathBuf,
    files: Vec<String>,
    dirs: Vec<String>,
    summary_str: String,
    children: Option<HashMap<String, DirectoryNode>>,
    num_listed_files: usize,
}

impl DirectoryNode {
    fn new(path: PathBuf, depth: usize, contents: &HashMap<PathBuf, DirContents>) -> Self {
        let (files, dirs) = contents
            .get(&path)
            .map(|c| (c.files.clone(), c.dirs.clone()))
            .unwrap_or_default();

        let mut node = Self {
            depth,
            path,
            files,
            dirs,
            summary_str: String::new(),
            children: None,
            num_listed_files: 0,
        };
        node.summary_str = node.get_remaining_str(&[], false, 3);
        node
    }

    fn get_remaining_str(
        &self,
        excluded_files: &[String],
        exclude_all_dirs: bool,
        k: usize,
    ) -> String {
        let remaining_files: Vec<String> = self
            .files
            .iter()
            .filter(|f| !excluded_files.contains(*f))
            .cloned()
            .collect();
        let mut file_ext_str = get_file_ext_str(&remaining_files, k);
        if !file_ext_str.is_empty() {
            file_ext_str.push(' ');
        }
        let file_count = remaining_files.len();
        let dir_count = if exclude_all_dirs { 0 } else { self.dirs.len() };
        let indent = "  ".repeat(self.depth + 1);
        format!("{indent}- [+{file_count} files {file_ext_str}& {dir_count} dirs]")
    }

    fn is_expanded(&self) -> bool {
        self.children.is_some()
    }

    fn expand_children(&mut self, contents: &HashMap<PathBuf, DirContents>) {
        if self.is_expanded() {
            return;
        }
        let mut children: HashMap<String, DirectoryNode> = HashMap::new();
        for dir in &self.dirs {
            let child_path = self.path.join(dir.trim_end_matches('/'));
            let dir_node = DirectoryNode::new(child_path, self.depth + 1, contents);
            children.insert(dir.clone(), dir_node);
        }
        self.children = Some(children);
        self.num_listed_files = std::cmp::min(3, self.files.len());
    }

    fn unexpand_children(&mut self) {
        self.children = None;
        self.num_listed_files = 0;
    }

    fn subitem_str(&self, subitem: &str) -> String {
        let indent = "  ".repeat(self.depth + 1);
        format!("{indent}- {subitem}")
    }

    fn get_complete_str(&self) -> String {
        assert!(self.is_expanded());
        let children = self.children.as_ref().unwrap();
        let mut remaining_subitems = self.dirs.clone();
        remaining_subitems.extend(self.files[0..self.num_listed_files].iter().cloned());
        remaining_subitems.sort_by_key(|s| s.to_lowercase());
        let mut curr_str = String::new();
        for subitem in remaining_subitems {
            curr_str.push_str(&self.subitem_str(&subitem));
            curr_str.push('\n');
            if let Some(child) = children.get(&subitem) {
                if child.is_expanded() {
                    curr_str.push_str(&child.get_complete_str());
                    curr_str.push('\n');
                } else {
                    curr_str.push_str(&child.summary_str);
                    curr_str.push('\n');
                }
            }
        }
        if self.files.len() > self.num_listed_files {
            let remaining_str =
                self.get_remaining_str(&self.files[0..self.num_listed_files], true, 3);
            curr_str.push_str(&remaining_str);
        }
        curr_str.trim_end_matches('\n').to_string()
    }
}

/// Project overview across every provisioned mount. One walk per root so a
/// multi-repo workspace prompt is not primary-only. Empty `roots` → empty string.
pub async fn list_contents_multi(
    roots: &[PathBuf],
    limits: ListContentsLimits,
) -> Result<String, FsError> {
    match roots {
        [] => Ok(String::new()),
        [only] => list_contents(only.clone(), limits).await,
        many => {
            let n = many.len().max(1);
            let per = ListContentsLimits {
                max_characters: limits.max_characters / n,
                max_depth: limits.max_depth,
                max_dirs_visited: limits.max_dirs_visited / n,
            };
            let mut out = String::new();
            for root in many {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&list_contents(root.clone(), per).await?);
            }
            Ok(out)
        }
    }
}

/// Creates the project overview
pub async fn list_contents(
    path: impl Into<PathBuf>,
    limits: ListContentsLimits,
) -> Result<String, FsError> {
    let _timer = (); // instrumentation_timer noop (dev infra)
    let t_total = Instant::now();
    let path: PathBuf = path.into();

    // Use this only for the printed header
    let path_head = {
        let s = path.to_string_lossy().replace('\\', "/");
        if s.ends_with('/') {
            s.to_owned()
        } else {
            format!("{s}/")
        }
    };

    let lim_characters = limits.max_characters + path_head.len();

    // Single walk to collect all directory contents
    let path_clone = path.clone();
    let max_depth = limits.max_depth;
    let max_dirs = limits.max_dirs_visited;
    let contents = {
        let _timer = (); // instrumentation_timer noop (dev infra)
        tokio::task::spawn_blocking(move || collect_all_contents(&path_clone, max_depth, max_dirs))
            .await
            .map_err(|e| FsError::Other(format!("walk join error: {e}")))?
    }?;

    let t_walk = t_total.elapsed();

    let (
        mut root_node,
        mut remaining_chars,
        to_fit_files,
        dirs_visited,
        max_depth_reached,
        depth_limit_hit,
        dirs_limit_hit,
    ) = {
        let _timer = (); // instrumentation_timer noop (dev infra)

        let mut root_node = DirectoryNode::new(path, 0, &contents);

        let min_chars = path_head.len() + root_node.summary_str.len();
        if min_chars > lim_characters {
            return Err(FsError::Other(format!(
                "Minimum possible string is too long for character limit, {} > {}",
                min_chars, lim_characters
            )));
        }

        let mut remaining_chars = lim_characters - min_chars;
        let mut to_fit_files = true;
        let mut dirs_visited: usize = 1; // Count root as visited
        let mut max_depth_reached: usize = 0;
        let mut depth_limit_hit = false;
        let mut dirs_limit_hit = false;
        let mut q: VecDeque<&mut DirectoryNode> = VecDeque::new();
        q.push_back(&mut root_node);
        while let Some(node) = q.pop_front() {
            // Check if we've hit the max directories limit
            if dirs_visited >= limits.max_dirs_visited {
                dirs_limit_hit = true;
                to_fit_files = false;
                break;
            }

            remaining_chars += node.summary_str.len();
            node.expand_children(&contents);
            dirs_visited += node.dirs.len();
            max_depth_reached = max_depth_reached.max(node.depth);

            let test_str = node.get_complete_str();
            let new_additional_len = test_str.replace('\n', "").len();
            if new_additional_len > remaining_chars {
                node.unexpand_children();
                to_fit_files = false;
                break;
            }
            remaining_chars -= new_additional_len;
            if let Some(children_map) = node.children.as_mut() {
                let mut child_values: Vec<&mut DirectoryNode> = children_map.values_mut().collect();
                child_values.sort_by_key(|node| node.path.to_string_lossy().to_lowercase());

                // Filter out children that exceed max_depth
                for child in child_values {
                    if child.depth > limits.max_depth {
                        depth_limit_hit = true;
                        continue;
                    }
                    q.push_back(child);
                }
            }
        }

        (
            root_node,
            remaining_chars,
            to_fit_files,
            dirs_visited,
            max_depth_reached,
            depth_limit_hit,
            dirs_limit_hit,
        )
    };

    {
        let _timer = (); // instrumentation_timer noop (dev infra)
        let mut q: VecDeque<&mut DirectoryNode> = VecDeque::new();
        if to_fit_files {
            q.push_back(&mut root_node);
        }
        let mut file_done = false;
        while let Some(node) = q.pop_front() {
            if !node.is_expanded() {
                continue;
            }
            let num_file_limit = node.files.len();
            for i in node.num_listed_files..num_file_limit {
                let new_additional_len = node.subitem_str(&node.files[i]).len();
                if new_additional_len <= remaining_chars {
                    node.num_listed_files = i + 1;
                    remaining_chars -= new_additional_len;
                } else {
                    file_done = true;
                    break;
                }
            }
            if file_done {
                break;
            }
            if let Some(children_map) = node.children.as_mut() {
                let mut child_values: Vec<&mut DirectoryNode> = children_map.values_mut().collect();
                child_values.sort_by_key(|node| node.path.to_string_lossy().to_lowercase());
                q.extend(child_values);
            }
        }
    }

    let output = {
        let _timer = (); // instrumentation_timer noop (dev infra)
        let mut output = format!("{path_head}\n");
        if root_node.is_expanded() {
            output.push_str(&root_node.get_complete_str());
        } else {
            output.push_str(&root_node.summary_str);
        }
        output
    };

    // Log warnings when limits are hit
    if depth_limit_hit {
        tracing::warn!(
            path = %path_head,
            max_depth = limits.max_depth,
            "list_contents: max_depth limit hit, some directories were not traversed"
        );
    }
    if dirs_limit_hit {
        tracing::warn!(
            path = %path_head,
            max_dirs_visited = limits.max_dirs_visited,
            dirs_visited = dirs_visited,
            "list_contents: max_dirs_visited limit hit, traversal stopped early"
        );
    }

    tracing::debug!(
        path = %path_head,
        max_characters = limits.max_characters,
        max_depth = limits.max_depth,
        max_dirs_visited = limits.max_dirs_visited,
        dirs_visited = dirs_visited,
        max_depth_reached = max_depth_reached,
        depth_limit_hit = depth_limit_hit,
        dirs_limit_hit = dirs_limit_hit,
        output_len = output.len(),
        walk_ms = t_walk.as_millis() as u64,
        elapsed_ms = t_total.elapsed().as_millis() as u64,
        "list_contents complete"
    );
    Ok(output)
}
