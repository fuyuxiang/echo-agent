//! Skip logic for copy operations (gitignore + additional patterns).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashSet;
use ignore::{WalkBuilder, WalkState};

/// Build a globset matcher for skip patterns.
pub(crate) fn build_skip_matcher(patterns: &[String]) -> Result<globset::GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(globset::Glob::new(pattern)?);
    }
    Ok(builder.build()?)
}

/// Collect all *unignored* paths in `source` (relative).
///
/// This is used to implement an "ignored-only" copy: by collecting unignored paths
/// and then skipping them during a second pass with `respect_gitignore=false`.
pub(crate) fn collect_unignored_paths(
    source: &Path,
    parallelism: usize,
) -> Result<DashSet<PathBuf>> {
    let unignored: Arc<DashSet<PathBuf>> = Arc::new(DashSet::new());

    // git_exclude/git_global off: external tools append broad patterns (e.g.
    // *.zip) to `.git/info/exclude`; the `ignore` crate would then drop matching
    // TRACKED files from the unignored set, so the ignored-copy clobbers them.
    let walker = WalkBuilder::new(source)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .threads(parallelism)
        .build_parallel();

    walker.run(|| {
        let unignored = Arc::clone(&unignored);
        Box::new(move |entry_result| {
            let entry = match entry_result {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };

            let rel_path = match entry.path().strip_prefix(source) {
                Ok(p) => p.to_path_buf(),
                Err(_) => return WalkState::Continue,
            };

            unignored.insert(rel_path);
            WalkState::Continue
        })
    });

    Ok(match Arc::try_unwrap(unignored) {
        Ok(set) => set,
        Err(arc) => {
            let mut set = DashSet::new();
            set.extend(arc.iter().map(|p| p.clone()));
            set
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
    use tempfile::TempDir;

    #[test]
    fn collect_unignored_includes_tracked_file_matching_git_exclude() {
        echo_agent_test_utils::require_git!();
        // A tracked file matching `.git/info/exclude` must stay "unignored" so
        // the ignored-copy doesn't re-copy and clobber it.
        let temp = TempDir::new().unwrap();
        let repo = temp.path();
        init_git_repo(repo);

        std::fs::write(repo.join("data.zip"), "tracked-archive").unwrap();
        std::fs::write(repo.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(repo.join(".gitignore"), "build/\n").unwrap();
        git_commit_all(repo, "initial");

        // External tooling appends broad patterns here (e.g., *.min.js, *.zip).
        // `git init` does not always create `.git/info/` (the hermetic git on
        // arm64 CI ships no init template), so create it before writing.
        let info_dir = repo.join(".git").join("info");
        std::fs::create_dir_all(&info_dir).unwrap();
        std::fs::write(info_dir.join("exclude"), "*.zip\n").unwrap();

        // A truly-ignored (gitignored, untracked) artifact.
        std::fs::create_dir(repo.join("build")).unwrap();
        std::fs::write(repo.join("build/out.o"), "obj").unwrap();

        let unignored = collect_unignored_paths(repo, 1).unwrap();

        assert!(
            unignored.contains(&PathBuf::from("data.zip")),
            "tracked file matching .git/info/exclude must be classed unignored"
        );
        assert!(unignored.contains(&PathBuf::from("main.rs")));
        assert!(
            !unignored.contains(&PathBuf::from("build/out.o")),
            "a real .gitignore'd file must remain ignored (not unignored)"
        );
    }
}
