//! Gitignore integration for AGENTS.md and skills discovery.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::{Path, PathBuf};

pub fn build_gitignore(repo_root: Option<&Path>) -> Option<Gitignore> {
    // No repo root → no gitignore rules to apply.
    let root = repo_root?;
    let mut builder = GitignoreBuilder::new(root);

    let repo_gitignore = root.join(".gitignore");
    if repo_gitignore.exists() {
        let _ = builder.add(&repo_gitignore);
    }

    if let Some(global_path) = get_global_gitignore_path()
        && global_path.exists()
    {
        let _ = builder.add(&global_path);
    }

    builder.build().ok()
}

pub fn is_ignored(path: &Path, gitignore: Option<&Gitignore>, repo_root: Option<&Path>) -> bool {
    let Some(gi) = gitignore else {
        return false;
    };
    echo_agent_tools::gitignore::is_ignored(gi, path, repo_root)
}

fn get_global_gitignore_path() -> Option<PathBuf> {
    git2::Config::open_default()
        .ok()
        .and_then(|cfg| cfg.get_path("core.excludesFile").ok())
        .or_else(|| echo_agent_config::discovery_home_dir().map(|h| h.join(".gitignore")))
}
