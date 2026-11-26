//! Git context collection for telemetry events.

pub struct GitContext {
    pub is_git_repo: bool,
}

pub fn collect_git_context(cwd: &str) -> GitContext {
    use git2::Repository;
    use std::path::Path;

    GitContext {
        is_git_repo: Repository::discover(Path::new(cwd)).is_ok(),
    }
}
