//! Shared test utilities for the pager crate.
//!
//! Compiled only in `#[cfg(test)]` builds. Import via `crate::test_util`.
use std::path::{Path, PathBuf};
/// Minimal `AgentView` for unit tests outside the dispatch/handler modules
/// (which keep their own richer factories).
pub fn make_agent_view(session_id: Option<&str>, cwd: &str) -> crate::app::agent_view::AgentView {
    use crate::app::agent::{AgentId, AgentSession, AgentState};
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let session = AgentSession {
        id: AgentId(0),
        acp_tx: tx,
        session_id: session_id.map(agent_client_protocol::SessionId::new),
        models: crate::acp::model_state::ModelState::default(),
        state: AgentState::Idle,
        tracker: crate::acp::tracker::AcpUpdateTracker::new(),
        cwd: std::path::PathBuf::from(cwd),
        is_worktree: false,
        forked_from: None,
        pending_prompts: std::collections::VecDeque::new(),
        next_queue_id: 0,
        yolo_mode: false,
        auto_mode: false,
        prompt_history: Vec::new(),
        prompt_history_loading: false,
        loading_replay: false,
        restore_degree: None,
        rate_limited: false,
        model_incompatible: false,
        credit_limit_blocked: false,
        free_usage_blocked: false,
        available_commands: Vec::new(),
        available_commands_generation: 0,
        available_tools: None,
        model_switch_pending: false,
        user_model_preference: None,
        deferred_model_switch: None,
        bg_tasks: std::collections::BTreeMap::new(),
        bg_tool_call_to_task: std::collections::HashMap::new(),
        scheduled_tasks: std::collections::HashMap::new(),
        in_flight_prompt: None,
        compact_held_prompt: None,
        current_prompt_id: None,
        created_via_new: false,
    };
    crate::app::agent_view::AgentView::new(
        session,
        crate::scrollback::state::ScrollbackState::new(),
    )
}
pub fn make_worktree_record(
    id: &str,
    path: &std::path::Path,
    label: &str,
) -> echo_agent_fast_worktree::WorktreeRecord {
    use echo_agent_fast_worktree::{WorktreeKind, WorktreeRecord, WorktreeStatus};
    WorktreeRecord {
        id: id.to_owned(),
        path: path.to_path_buf(),
        source_repo: "/repo".into(),
        repo_name: "repo".into(),
        kind: WorktreeKind::Session,
        creation_mode: "linked".into(),
        git_ref: None,
        head_commit: None,
        session_id: None,
        creator_pid: None,
        created_at: 0,
        last_accessed_at: None,
        status: WorktreeStatus::Alive,
        metadata: Some(serde_json::json!({ "label": label })),
    }
}
/// Every row containing `row_marker` starts its PATH cell at the header's
/// PATH column, measured in display width so CJK regressions fail.
pub fn assert_path_column_aligned(text: &str, row_marker: &str) {
    use unicode_width::UnicodeWidthStr;
    let lines: Vec<&str> = text.lines().collect();
    let header = lines
        .iter()
        .find(|l| l.ends_with("PATH"))
        .unwrap_or_else(|| panic!("no PATH header in: {text}"));
    let path_col = header.width() - "PATH".width();
    let mut rows = 0;
    for line in lines.iter().filter(|l| l.contains(row_marker)) {
        let (_, path) = line
            .rsplit_once(' ')
            .expect("rows end in a space-free test path (PATH is the last cell)");
        assert_eq!(
            line.width() - path.width(),
            path_col,
            "path column must stay width-aligned: {line:?}"
        );
        rows += 1;
    }
    assert!(rows > 0, "no table rows matched {row_marker:?} in: {text}");
}
/// RAII guard for temporarily overriding an environment variable.
///
/// Captures the original value on construction and restores it on drop.
/// Used by theme and persist tests to redirect `HOME`/`USERPROFILE` to
/// temp directories without affecting the real user config.
pub struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}
impl EnvVarGuard {
    /// Override `key` to `value` (paths, URLs, flags — anything OsStr-able),
    /// returning a guard that restores the original on drop.
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.original {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}
/// Shared ECHO_AGENT_HOME boundary fixture for the resume-by-title startup and
/// pre-sandbox tests.
///
/// `echo_agent_home()` is OnceLock-cached process-wide, so summaries land under the
/// *resolved* home (possibly the real `~/.echo-agent` when another test pinned the
/// cache first); cwd-encoded dirnames are tempdir-unique, and cleanup runs on
/// drop so it survives assertion panics. Callers must hold
/// `#[serial_test::serial(ECHO_AGENT_HOME)]`.
pub struct EchoAgentHomeFixture {
    _home: tempfile::TempDir,
    cwd: tempfile::TempDir,
    cleanup: Vec<std::path::PathBuf>,
}
impl Drop for EchoAgentHomeFixture {
    fn drop(&mut self) {
        for dir in &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
impl Default for EchoAgentHomeFixture {
    fn default() -> Self {
        Self::new()
    }
}
impl EchoAgentHomeFixture {
    pub fn new() -> Self {
        let home = tempfile::tempdir().expect("home tempdir");
        unsafe { std::env::set_var("ECHO_AGENT_HOME", home.path()) };
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        Self {
            _home: home,
            cwd,
            cleanup: Vec::new(),
        }
    }
    /// Canonicalized so the summary cwd encoding matches what production
    /// path resolution sees (macOS tempdirs are symlinked). Tests pass this
    /// through the explicit `*_for_cwd` seams; the process cwd is never
    /// mutated.
    pub fn cwd_str(&self) -> String {
        dunce::canonicalize(self.cwd.path())
            .expect("canonicalize cwd")
            .to_string_lossy()
            .to_string()
    }
    /// Write a minimal valid summary.json (every non-defaulted `Summary`
    /// field) for `id` under `cwd`, merging `extra` fields on top.
    pub fn write_summary(&mut self, cwd: &str, id: &str, extra: serde_json::Value) {
        let sessions_cwd_dir = Self::sessions_cwd_dir(cwd);
        if !self.cleanup.contains(&sessions_cwd_dir) {
            self.cleanup.push(sessions_cwd_dir.clone());
        }
        let dir = sessions_cwd_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut v = serde_json::json!({
            "info": { "id": id, "cwd": cwd },
            "session_summary": "auto summary",
            "created_at": "2026-07-01T00:00:00Z",
            "updated_at": "2026-07-01T00:00:00Z",
            "num_messages": 1,
            "current_model_id": "echo-agent-build",
        });
        if let Some(map) = extra.as_object() {
            for (k, val) in map {
                v[k.as_str()] = val.clone();
            }
        }
        std::fs::write(dir.join("summary.json"), serde_json::to_vec(&v).unwrap()).unwrap();
    }
    /// Delete a previously written session dir (concurrent-delete simulation).
    pub fn remove_session(&self, cwd: &str, id: &str) {
        let _ = std::fs::remove_dir_all(Self::sessions_cwd_dir(cwd).join(id));
    }
    fn sessions_cwd_dir(cwd: &str) -> std::path::PathBuf {
        let encoded = echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(cwd);
        echo_agent_runtime::util::echo_agent_home::echo_agent_home()
            .join("sessions")
            .join(&encoded)
    }
}
/// On-disk git checkout living under a tempdir. Dropping the fixture deletes it.
pub struct TempGitRepo {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
}
impl TempGitRepo {
    pub fn init(branch: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp git root");
        let path = dir.path().join("repo");
        init_git_repo_on_branch(&path, branch);
        Self { _dir: dir, path }
    }
    /// CoW-style standalone clone: `.git` is a directory plus `echo-agent-worktree-source`.
    pub fn standalone_clone(&self, branch: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp clone root");
        let path = dir.path().join("clone");
        copy_dir_all(&self.path, &path);
        checkout_named_branch(&path, branch);
        std::fs::write(
            path.join(".git").join("echo-agent-worktree-source"),
            self.path.to_string_lossy().as_bytes(),
        )
        .unwrap();
        Self { _dir: dir, path }
    }
    /// Linked `git worktree` of `branch` as a sibling of this checkout.
    /// `.git` at the returned path is a file. Keep `self` alive while using it.
    pub fn add_linked_worktree(&self, name: &str, branch: &str) -> PathBuf {
        let repo = git2::Repository::open(&self.path).unwrap();
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        if repo.find_branch(branch, git2::BranchType::Local).is_err() {
            repo.branch(branch, &commit, false).unwrap();
        }
        let wt_path = self
            .path
            .parent()
            .expect("repo lives in a temp parent")
            .join(name);
        let reference = repo
            .find_branch(branch, git2::BranchType::Local)
            .unwrap()
            .into_reference();
        repo.worktree(
            name,
            &wt_path,
            Some(git2::WorktreeAddOptions::new().reference(Some(&reference))),
        )
        .unwrap();
        wt_path
    }
}
/// Match [`crate::git_info`] tilde-collapse of a filesystem path.
pub fn collapsed_path_display(path: &Path) -> String {
    let home = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h.trim_end_matches(['/', '\\'])));
    match home {
        Some(h) => path
            .strip_prefix(&h)
            .map(|rest| format!("~/{}", rest.display()))
            .unwrap_or_else(|_| path.display().to_string()),
        None => path.display().to_string(),
    }
}
pub fn init_git_repo_on_branch(path: &Path, branch: &str) {
    std::fs::create_dir_all(path).unwrap();
    let repo = git2::Repository::init(path).unwrap();
    std::fs::write(path.join("README"), "test\n").unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let tree_id = {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        index.write_tree().unwrap()
    };
    let tree = repo.find_tree(tree_id).unwrap();
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    let commit = repo.find_commit(oid).unwrap();
    let current = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(str::to_string));
    if current.as_deref() != Some(branch) {
        repo.branch(branch, &commit, true).unwrap();
        repo.set_head(&format!("refs/heads/{branch}")).unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
    }
}
fn checkout_named_branch(repo_path: &Path, branch: &str) {
    let repo = git2::Repository::open(repo_path).unwrap();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    if repo.find_branch(branch, git2::BranchType::Local).is_err() {
        repo.branch(branch, &commit, false).unwrap();
    }
    repo.set_head(&format!("refs/heads/{branch}")).unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
}
fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}
