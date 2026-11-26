use std::path::{Path, PathBuf};
use std::time::Duration;

use echo_agent_workspace_types::rpc::export_github::{ExportGithubError, ExportGithubResponse};

pub const GITHUB_REPO_MAPPING_FILE: &str = ".github_repo";

const EXPORT_BUDGET: Duration = Duration::from_secs(120);

const DEFAULT_BRANCH: &str = "main";
const AUTHOR_NAME: &str = "EchoAgent";
const AUTHOR_EMAIL: &str = "echo-agent-export@users.noreply.github.com";

const SEED_GITIGNORE: &str = "node_modules/\n.project_id\n.github_repo\n.env\n.env.*\n";

pub struct ExportGithubParams<'a> {
    pub project_dir: &'a Path,
    pub repo_full_name: Option<&'a str>,
    pub remote_url_base: &'a str,
    pub web_url_base: &'a str,
    pub branch: Option<&'a str>,
    pub commit_message: Option<&'a str>,
}

#[derive(Debug)]
pub struct ExportGithubFailure {
    pub kind: ExportGithubError,
    pub message: String,
}

impl ExportGithubFailure {
    fn new(kind: ExportGithubError, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn validate_repo_full_name(name: &str) -> Result<(), ExportGithubFailure> {
    let parts: Vec<&str> = name.split('/').collect();
    let valid_segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if parts.len() == 2 && parts.iter().all(|p| valid_segment(p)) {
        Ok(())
    } else {
        Err(ExportGithubFailure::new(
            ExportGithubError::InvalidRepoName,
            format!("repository must be 'owner/name', got {name:?}"),
        ))
    }
}

pub async fn run_export(
    params: ExportGithubParams<'_>,
) -> Result<ExportGithubResponse, ExportGithubFailure> {
    let dir = params.project_dir.to_owned();
    match tokio::time::timeout(EXPORT_BUDGET, run_export_inner(params)).await {
        Ok(result) => result,
        Err(_) => {
            remove_stale_git_locks(&dir);
            Err(ExportGithubFailure::new(
                ExportGithubError::Timeout,
                format!("export exceeded {}s", EXPORT_BUDGET.as_secs()),
            ))
        }
    }
}

fn remove_stale_git_locks(dir: &Path) {
    for lock in ["index.lock", "HEAD.lock", "config.lock"] {
        let path = dir.join(".git").join(lock);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::warn!(lock = %path.display(), "removed stale git lock after timeout");
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(lock = %path.display(), error = %e, "failed to remove git lock");
            }
        }
    }
}

async fn run_export_inner(
    params: ExportGithubParams<'_>,
) -> Result<ExportGithubResponse, ExportGithubFailure> {
    let dir = params.project_dir;
    if !dir.is_dir() {
        return Err(ExportGithubFailure::new(
            ExportGithubError::ProjectDirInvalid,
            format!("project dir does not exist: {}", dir.display()),
        ));
    }

    let repo_full_name = resolve_repo_mapping(dir, non_empty(params.repo_full_name))?;
    let remote_url = format!(
        "{}/{}.git",
        params.remote_url_base.trim_end_matches('/'),
        repo_full_name
    );

    ensure_repo(dir, non_empty(params.branch)).await?;
    seed_gitignore_if_absent(dir)?;
    ensure_export_excludes(dir)?;
    untrack_excluded_paths(dir).await?;

    git(dir, &["add", "-A"]).await?;
    let no_changes = commit_if_dirty(dir, non_empty(params.commit_message)).await?;
    let branch = current_branch(dir).await?;
    let commit_sha = match git(dir, &["rev-parse", "HEAD"]).await {
        Ok(sha) => sha,
        Err(_) if no_changes => {
            return Err(ExportGithubFailure::new(
                ExportGithubError::GitFailed,
                "repository has no commits and no changes to export",
            ));
        }
        Err(e) => return Err(e),
    };

    set_remote(dir, &remote_url).await?;
    push(dir, &branch).await?;

    Ok(ExportGithubResponse {
        repo_url: format!(
            "{}/{}",
            params.web_url_base.trim_end_matches('/'),
            repo_full_name
        ),
        repo_full_name,
        branch,
        commit_sha,
        no_changes,
    })
}

fn resolve_repo_mapping(
    dir: &Path,
    requested: Option<&str>,
) -> Result<String, ExportGithubFailure> {
    let mapping_path = dir.join(GITHUB_REPO_MAPPING_FILE);
    let stored = std::fs::read_to_string(&mapping_path)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_owned()))
        .filter(|s| !s.is_empty());

    let repo = match (requested, stored) {
        (Some(req), _) => req.to_owned(),
        (None, Some(stored)) => stored,
        (None, None) => {
            return Err(ExportGithubFailure::new(
                ExportGithubError::RepoNotSpecified,
                "no repository named and no .github_repo mapping exists",
            ));
        }
    };
    validate_repo_full_name(&repo)?;

    std::fs::write(&mapping_path, format!("{repo}\n")).map_err(|e| {
        ExportGithubFailure::new(
            ExportGithubError::GitFailed,
            format!("writing {GITHUB_REPO_MAPPING_FILE}: {e}"),
        )
    })?;
    Ok(repo)
}

async fn ensure_repo(dir: &Path, branch: Option<&str>) -> Result<(), ExportGithubFailure> {
    if !dir.join(".git").exists() {
        git(dir, &["init", "-b", branch.unwrap_or(DEFAULT_BRANCH)]).await?;
        return Ok(());
    }
    let Some(requested) = branch else {
        return Ok(());
    };
    if git(dir, &["rev-parse", "--verify", "HEAD"]).await.is_ok() {
        git(dir, &["checkout", "-B", requested]).await?;
    } else {
        let target = format!("refs/heads/{requested}");
        git(dir, &["symbolic-ref", "HEAD", &target]).await?;
    }
    Ok(())
}

fn seed_gitignore_if_absent(dir: &Path) -> Result<(), ExportGithubFailure> {
    let path = dir.join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    std::fs::write(&path, SEED_GITIGNORE).map_err(|e| {
        ExportGithubFailure::new(
            ExportGithubError::GitFailed,
            format!("seeding .gitignore: {e}"),
        )
    })
}

fn ensure_export_excludes(dir: &Path) -> Result<(), ExportGithubFailure> {
    let io_err = |e: std::io::Error| {
        ExportGithubFailure::new(
            ExportGithubError::GitFailed,
            format!("writing .git/info/exclude: {e}"),
        )
    };
    let info_dir = dir.join(".git").join("info");
    std::fs::create_dir_all(&info_dir).map_err(io_err)?;
    let exclude_path = info_dir.join("exclude");
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    let missing: Vec<&str> = SEED_GITIGNORE
        .lines()
        .filter(|rule| !existing.lines().any(|line| line.trim() == *rule))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    std::fs::write(
        &exclude_path,
        format!("{existing}{separator}{}\n", missing.join("\n")),
    )
    .map_err(io_err)
}

async fn untrack_excluded_paths(dir: &Path) -> Result<(), ExportGithubFailure> {
    let mut args = vec![
        "rm",
        "-r",
        "-f",
        "--cached",
        "--ignore-unmatch",
        "--quiet",
        "--",
    ];
    args.extend(SEED_GITIGNORE.lines());
    git(dir, &args).await?;
    Ok(())
}

async fn commit_if_dirty(dir: &Path, message: Option<&str>) -> Result<bool, ExportGithubFailure> {
    let status = git(dir, &["status", "--porcelain"]).await?;
    if status.is_empty() {
        return Ok(true);
    }
    let message = message.unwrap_or("Export from EchoAgent");
    git(dir, &["commit", "-m", message]).await?;
    Ok(false)
}

async fn current_branch(dir: &Path) -> Result<String, ExportGithubFailure> {
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    if branch == "HEAD" {
        Ok(DEFAULT_BRANCH.to_owned())
    } else {
        Ok(branch)
    }
}

async fn set_remote(dir: &Path, url: &str) -> Result<(), ExportGithubFailure> {
    if git(dir, &["remote", "get-url", "origin"]).await.is_ok() {
        git(dir, &["remote", "set-url", "origin", url]).await?;
    } else {
        git(dir, &["remote", "add", "origin", url]).await?;
    }
    Ok(())
}

async fn push(dir: &Path, branch: &str) -> Result<(), ExportGithubFailure> {
    let refspec = format!("HEAD:refs/heads/{branch}");
    match git(dir, &["push", "-u", "origin", &refspec]).await {
        Ok(_) => Ok(()),
        Err(failure) => Err(classify_push_failure(failure)),
    }
}

const PUSH_REJECTED_MARKERS: [&str; 5] = [
    "non-fast-forward",
    "fetch first",
    "[rejected]",
    "updates were rejected",
    "failed to push some refs",
];

const PUSH_AUTH_MARKERS: [&str; 8] = [
    "authentication failed",
    "could not read username",
    "permission to",
    "http basic: access denied",
    "invalid username or token",
    "authentication required",
    "returned error: 403",
    "returned error: 401",
];

fn classify_push_failure(failure: ExportGithubFailure) -> ExportGithubFailure {
    let lower = failure.message.to_lowercase();
    let kind = if PUSH_AUTH_MARKERS.iter().any(|m| lower.contains(m)) {
        ExportGithubError::AuthFailed
    } else if PUSH_REJECTED_MARKERS.iter().any(|m| lower.contains(m)) {
        ExportGithubError::PushRejected
    } else {
        tracing::warn!(message = %failure.message, "git push failed with unclassified stderr");
        ExportGithubError::GitFailed
    };
    ExportGithubFailure::new(kind, failure.message)
}

async fn git(dir: &Path, args: &[&str]) -> Result<String, ExportGithubFailure> {
    let mut cmd = echo_agent_tty_utils::git_command();
    cmd.current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL)
        .args(args);
    let mut async_cmd = tokio::process::Command::from(cmd);
    async_cmd.kill_on_drop(true);
    let output = async_cmd.output().await.map_err(|e| {
        ExportGithubFailure::new(
            ExportGithubError::GitFailed,
            format!("spawning git {}: {e}", args.join(" ")),
        )
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(ExportGithubFailure::new(
            ExportGithubError::GitFailed,
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ))
    }
}

pub fn mapping_file_path(project_dir: &Path) -> PathBuf {
    project_dir.join(GITHUB_REPO_MAPPING_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gh-export-test-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bare_remote(base: &Path, full_name: &str) -> String {
        let repo_path = base.join(format!("{full_name}.git"));
        std::fs::create_dir_all(&repo_path).unwrap();
        let mut cmd = echo_agent_tty_utils::git_command();
        let out = cmd
            .args(["init", "--bare"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        format!("file://{}", base.display())
    }

    fn params<'a>(
        project: &'a Path,
        repo: Option<&'a str>,
        base: &'a str,
    ) -> ExportGithubParams<'a> {
        ExportGithubParams {
            project_dir: project,
            repo_full_name: repo,
            remote_url_base: base,
            web_url_base: "https://github.com",
            branch: None,
            commit_message: None,
        }
    }

    fn remote_head(base: &Path, full_name: &str) -> String {
        let mut cmd = echo_agent_tty_utils::git_command();
        let out = cmd
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(base.join(format!("{full_name}.git")))
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[tokio::test]
    async fn first_export_inits_commits_and_pushes() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        let project = tmp_dir("project");
        std::fs::write(project.join("index.html"), "<html></html>").unwrap();

        let res = run_export(params(&project, Some("user/app"), &base))
            .await
            .unwrap();

        assert_eq!(res.repo_full_name, "user/app");
        assert_eq!(res.repo_url, "https://github.com/user/app");
        assert_eq!(res.branch, "main");
        assert!(!res.no_changes);
        assert_eq!(remote_head(&remote_base, "user/app"), res.commit_sha);
        assert_eq!(
            std::fs::read_to_string(project.join(GITHUB_REPO_MAPPING_FILE)).unwrap(),
            "user/app\n"
        );
        assert!(project.join(".gitignore").exists());
    }

    #[test]
    fn stale_git_locks_are_removed_after_timeout() {
        let project = tmp_dir("project");
        let git_dir = project.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        for lock in ["index.lock", "HEAD.lock", "config.lock"] {
            std::fs::write(git_dir.join(lock), "").unwrap();
        }
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        remove_stale_git_locks(&project);

        for lock in ["index.lock", "HEAD.lock", "config.lock"] {
            assert!(!git_dir.join(lock).exists(), "{lock} should be removed");
        }
        assert!(git_dir.join("HEAD").exists());
    }

    #[tokio::test]
    async fn empty_branch_and_commit_message_fall_back_to_defaults() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        let project = tmp_dir("project");
        std::fs::write(project.join("index.html"), "<html></html>").unwrap();

        let mut p = params(&project, Some("user/app"), &base);
        p.branch = Some("");
        p.commit_message = Some("  ");
        let res = run_export(p).await.unwrap();

        assert_eq!(res.branch, "main");
        assert!(!res.no_changes);
        assert_eq!(remote_head(&remote_base, "user/app"), res.commit_sha);
    }

    #[tokio::test]
    async fn preexisting_gitignore_still_excludes_sensitive_files() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        let project = tmp_dir("project");
        std::fs::write(project.join(".gitignore"), "dist/\n").unwrap();
        std::fs::write(project.join("index.html"), "<html></html>").unwrap();
        std::fs::write(project.join(".env"), "SECRET=1").unwrap();

        run_export(params(&project, Some("user/app"), &base))
            .await
            .unwrap();

        let tracked = git(&project, &["ls-tree", "-r", "--name-only", "HEAD"])
            .await
            .unwrap();
        assert!(tracked.contains("index.html"));
        assert!(!tracked.contains(".env"), "committed .env: {tracked}");
        assert!(
            !tracked.contains(GITHUB_REPO_MAPPING_FILE),
            "committed mapping file: {tracked}"
        );
        assert_eq!(
            std::fs::read_to_string(project.join(".gitignore")).unwrap(),
            "dist/\n"
        );
    }

    #[tokio::test]
    async fn second_export_reuses_mapping_and_reports_no_changes() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        let project = tmp_dir("project");
        std::fs::write(project.join("main.py"), "print('hi')").unwrap();

        let first = run_export(params(&project, Some("user/app"), &base))
            .await
            .unwrap();
        let second = run_export(params(&project, None, &base)).await.unwrap();

        assert_eq!(second.repo_full_name, "user/app");
        assert!(second.no_changes);
        assert_eq!(second.commit_sha, first.commit_sha);
    }

    #[tokio::test]
    async fn changed_files_produce_a_new_commit_on_the_same_repo() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        let project = tmp_dir("project");
        std::fs::write(project.join("a.txt"), "one").unwrap();

        let first = run_export(params(&project, Some("user/app"), &base))
            .await
            .unwrap();
        std::fs::write(project.join("a.txt"), "two").unwrap();
        let second = run_export(params(&project, None, &base)).await.unwrap();

        assert!(!second.no_changes);
        assert_ne!(second.commit_sha, first.commit_sha);
        assert_eq!(remote_head(&remote_base, "user/app"), second.commit_sha);
    }

    #[tokio::test]
    async fn request_repo_overrides_stale_mapping() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        bare_remote(&remote_base, "user/other");
        let project = tmp_dir("project");
        std::fs::write(project.join("a.txt"), "one").unwrap();

        run_export(params(&project, Some("user/app"), &base))
            .await
            .unwrap();
        let res = run_export(params(&project, Some("user/other"), &base))
            .await
            .unwrap();

        assert_eq!(res.repo_full_name, "user/other");
        assert_eq!(
            std::fs::read_to_string(project.join(GITHUB_REPO_MAPPING_FILE)).unwrap(),
            "user/other\n"
        );
    }

    #[tokio::test]
    async fn missing_repo_and_mapping_is_a_typed_error() {
        let project = tmp_dir("project");
        std::fs::write(project.join("a.txt"), "one").unwrap();
        for repo in [None, Some(""), Some("  ")] {
            let err = run_export(params(&project, repo, "file:///nowhere"))
                .await
                .unwrap_err();
            assert_eq!(
                err.kind,
                ExportGithubError::RepoNotSpecified,
                "for {repo:?}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_repo_name_is_rejected_before_any_git_runs() {
        let project = tmp_dir("project");
        std::fs::write(project.join("a.txt"), "one").unwrap();
        for bad in ["justname", "a/b/c", "../evil/repo", "owner/na me"] {
            let err = run_export(params(&project, Some(bad), "file:///nowhere"))
                .await
                .unwrap_err();
            assert_eq!(err.kind, ExportGithubError::InvalidRepoName, "for {bad:?}");
        }
        assert!(!project.join(".git").exists());
    }

    #[tokio::test]
    async fn existing_gitignore_is_not_overwritten() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        let project = tmp_dir("project");
        std::fs::write(project.join("a.txt"), "one").unwrap();
        std::fs::write(project.join(".gitignore"), "custom/\n").unwrap();

        run_export(params(&project, Some("user/app"), &base))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(project.join(".gitignore")).unwrap(),
            "custom/\n"
        );
    }

    #[tokio::test]
    async fn non_fast_forward_push_maps_to_push_rejected() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");

        let project_a = tmp_dir("project-a");
        std::fs::write(project_a.join("a.txt"), "one").unwrap();
        run_export(params(&project_a, Some("user/app"), &base))
            .await
            .unwrap();

        let project_b = tmp_dir("project-b");
        std::fs::write(project_b.join("b.txt"), "unrelated history").unwrap();
        let err = run_export(params(&project_b, Some("user/app"), &base))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ExportGithubError::PushRejected);
    }

    #[tokio::test]
    async fn previously_tracked_secrets_are_untracked_on_export() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        let project = tmp_dir("project");
        std::fs::write(project.join("index.html"), "<html></html>").unwrap();
        std::fs::write(project.join(".env"), "SECRET=1").unwrap();
        std::fs::write(project.join(".project_id"), "proj-123").unwrap();
        git(&project, &["init", "-b", "main"]).await.unwrap();
        git(&project, &["add", "-A"]).await.unwrap();
        git(&project, &["commit", "-m", "seed"]).await.unwrap();

        let res = run_export(params(&project, Some("user/app"), &base))
            .await
            .unwrap();

        let tracked = git(&project, &["ls-tree", "-r", "--name-only", "HEAD"])
            .await
            .unwrap();
        assert!(tracked.contains("index.html"));
        assert!(!tracked.contains(".env"), "still tracks .env: {tracked}");
        assert!(
            !tracked.contains(".project_id"),
            "still tracks .project_id: {tracked}"
        );
        assert!(!res.no_changes);
        assert!(project.join(".env").exists());
        assert!(project.join(".project_id").exists());
    }

    #[tokio::test]
    async fn requested_branch_is_used_on_reexport() {
        let remote_base = tmp_dir("remote");
        let base = bare_remote(&remote_base, "user/app");
        let project = tmp_dir("project");
        std::fs::write(project.join("a.txt"), "one").unwrap();

        run_export(params(&project, Some("user/app"), &base))
            .await
            .unwrap();

        std::fs::write(project.join("a.txt"), "two").unwrap();
        let mut second = params(&project, None, &base);
        second.branch = Some("feature");
        let res = run_export(second).await.unwrap();

        assert_eq!(res.branch, "feature");
        let mut cmd = echo_agent_tty_utils::git_command();
        let out = cmd
            .args(["rev-parse", "refs/heads/feature"])
            .current_dir(remote_base.join("user/app.git"))
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), res.commit_sha);
    }

    #[test]
    fn push_failure_stderr_classification_table() {
        let cases = [
            (
                "! [rejected] main -> main (non-fast-forward)",
                ExportGithubError::PushRejected,
            ),
            (
                "hint: Updates were rejected because the remote contains work that you do not have locally",
                ExportGithubError::PushRejected,
            ),
            (
                "error: failed to push some refs to 'https://github.com/user/app.git'",
                ExportGithubError::PushRejected,
            ),
            (
                "fatal: Authentication failed for 'https://github.com/user/app.git/'",
                ExportGithubError::AuthFailed,
            ),
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
                ExportGithubError::AuthFailed,
            ),
            (
                "remote: Permission to user/app.git denied to some-bot.",
                ExportGithubError::AuthFailed,
            ),
            (
                "The requested URL returned error: 403",
                ExportGithubError::AuthFailed,
            ),
            (
                "The requested URL returned error: 401",
                ExportGithubError::AuthFailed,
            ),
            (
                "remote: HTTP Basic: Access denied",
                ExportGithubError::AuthFailed,
            ),
            (
                "remote: Invalid username or token.",
                ExportGithubError::AuthFailed,
            ),
            (
                "fatal: Authentication required",
                ExportGithubError::AuthFailed,
            ),
            (
                "remote: Permission to user/app.git denied to some-bot.\nfatal: unable to access 'https://github.com/user/app.git/': The requested URL returned error: 403\nerror: failed to push some refs to 'https://github.com/user/app.git'",
                ExportGithubError::AuthFailed,
            ),
            (
                "fatal: Authentication failed for 'https://github.com/user/app.git/'\nerror: failed to push some refs to 'https://github.com/user/app.git'",
                ExportGithubError::AuthFailed,
            ),
            (
                "! [rejected] main -> main (fetch first)\nerror: failed to push some refs to 'https://github.com/user403/app401.git'\nhint: Updates were rejected because the remote contains work that you do not have locally",
                ExportGithubError::PushRejected,
            ),
            (
                "fatal: unable to access 'https://github.com/user/app.git/': Could not resolve host: github.com",
                ExportGithubError::GitFailed,
            ),
        ];
        for (stderr, expected) in cases {
            let failure = classify_push_failure(ExportGithubFailure::new(
                ExportGithubError::GitFailed,
                stderr.to_owned(),
            ));
            assert_eq!(failure.kind, expected, "for {stderr:?}");
        }
    }

    #[tokio::test]
    async fn missing_project_dir_is_a_typed_error() {
        let ghost = std::env::temp_dir().join(format!("gh-export-ghost-{}", uuid::Uuid::new_v4()));
        let err = run_export(params(&ghost, Some("user/app"), "file:///nowhere"))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ExportGithubError::ProjectDirInvalid);
    }
}
