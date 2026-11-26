//! Git methods (`workspace.git_*`, `workspace.detect_vcs_kind`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RpcActivityClass, WorkspaceRpc};

/// `workspace.git_status`. The response value is a JSON string (branch,
/// ahead/behind, staged files), capped server-side at ~1 KB.
///
/// **DEPRECATED**: This method is deprecated and will be removed in a future
/// release. Use [`GitStatusExtReq`] with `format: GitStatusFormat::Prompt`
/// instead, which provides the same compact JSON string output.
///
/// Migration:
/// ```ignore
/// // Old (deprecated):
/// let status: serde_json::Value = client.git_status().await?;
///
/// // New (recommended):
/// let response = client.git_status_ext(&GitStatusExtReq {
///     format: GitStatusFormat::Prompt,
///     ..Default::default()
/// }).await?;
/// let status = response.prompt.expect("prompt format should have prompt");
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitStatusReq {}

impl WorkspaceRpc for GitStatusReq {
    const METHOD: &'static str = "workspace.git_status";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatusExtReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    #[serde(default)]
    pub include_untracked: bool,
    #[serde(default)]
    pub include_stats: bool,
    #[serde(default = "default_true")]
    pub ignore_submodules: bool,
    #[serde(default)]
    pub include_patches: bool,

    /// Output format: "structured" (default) or "prompt" (compact plain-text status).
    #[serde(default)]
    pub format: GitStatusFormat,
}

// Manual `Default` (not derived) so it matches serde: `ignore_submodules`
// defaults to `true`, which a derived `Default` would set to `false`.
// Omitted `include_untracked` is false so sloppy clients skip the expensive walk.
impl Default for GitStatusExtReq {
    fn default() -> Self {
        Self {
            git_root: None,
            include_untracked: false,
            include_stats: false,
            ignore_submodules: true,
            include_patches: false,
            format: GitStatusFormat::default(),
        }
    }
}

impl WorkspaceRpc for GitStatusExtReq {
    const METHOD: &'static str = "workspace.git_status_ext";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = GitStatusExtResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFilesReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    pub paths: Vec<String>,
    #[serde(default = "default_head")]
    pub version: String,
}

impl WorkspaceRpc for GitFilesReq {
    const METHOD: &'static str = "workspace.git_files";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = GitReadFilesData;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitDiffReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    pub paths: Option<Vec<String>>,
    #[serde(default = "default_head")]
    pub from: String,
    #[serde(default = "default_working")]
    pub to: String,
    #[serde(default)]
    pub include_patch: bool,
    #[serde(default)]
    pub include_content: bool,
    #[serde(default)]
    pub merge_base: bool,
}

impl WorkspaceRpc for GitDiffReq {
    const METHOD: &'static str = "workspace.git_diff";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = GitDiffsData;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStageReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    pub paths: Option<Vec<String>>,
}

impl WorkspaceRpc for GitStageReq {
    const METHOD: &'static str = "workspace.git_stage";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = StageData;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStageContentReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    pub path: String,
    pub content: String,
}

impl WorkspaceRpc for GitStageContentReq {
    const METHOD: &'static str = "workspace.git_stage_content";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = ();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitUnstageReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    pub paths: Option<Vec<String>>,
}

impl WorkspaceRpc for GitUnstageReq {
    const METHOD: &'static str = "workspace.git_unstage";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = ();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitDiscardReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub scope: DiscardScope,
    #[serde(default)]
    pub include_untracked: bool,
}

impl WorkspaceRpc for GitDiscardReq {
    const METHOD: &'static str = "workspace.git_discard";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = ();
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitCommitReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    pub message: String,
    #[serde(default)]
    pub amend: bool,
    #[serde(default)]
    pub signoff: bool,
    #[serde(default)]
    pub push: bool,
    #[serde(default)]
    pub sync: bool,
    /// Stage everything (`git add -A`, honoring `.gitignore` and
    /// `info/exclude`) before committing. With this set, a tree with nothing
    /// to commit is a result (`CommitOutcome::clean`), not an error, and the
    /// push step still runs — so a retry can deliver an earlier unpushed
    /// commit. Without it, committing with nothing staged is an error.
    #[serde(default)]
    pub stage_all: bool,
    /// Seed the local-only default excludes (`.env`, `node_modules/`, build
    /// output, …) into `info/exclude` before staging, so `stage_all` can
    /// never sweep them in. Idempotent: guarded by a marker line, shared
    /// with environments that pre-seed the same block.
    #[serde(default)]
    pub seed_default_excludes: bool,
    /// Refuse to commit unless the workspace is on exactly this branch
    /// (detached HEAD never matches). Guards single-writer conversation
    /// branches.
    #[serde(default)]
    pub expected_branch: Option<String>,
}

impl WorkspaceRpc for GitCommitReq {
    const METHOD: &'static str = "workspace.git_commit";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = CommitResult;
}

/// Merge the base branch into the current (conversation) branch. Merge,
/// never rebase — conv-branch history must not be rewritten. On conflicts
/// the merge is left in progress so an agent can resolve and commit it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitSyncBaseReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    /// Ref on `origin` to merge in. `None` ⇒ the remote's default branch
    /// (`git fetch origin HEAD`).
    #[serde(default)]
    pub base_ref: Option<String>,
    /// Roll back an in-progress merge (`git merge --abort`) instead of
    /// merging. Idempotent: no merge in progress is still `Aborted`.
    #[serde(default)]
    pub abort: bool,
    /// Refuse to merge unless the workspace is on exactly this branch
    /// (detached HEAD never matches), mirroring `GitCommitReq`. Ignored for
    /// `abort`, which must stay usable as a rollback wherever the merge
    /// happened.
    #[serde(default)]
    pub expected_branch: Option<String>,
}

impl WorkspaceRpc for GitSyncBaseReq {
    const METHOD: &'static str = "workspace.git_sync_base";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = GitSyncBaseResult;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitSyncBaseResult {
    pub outcome: GitSyncBaseOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitSyncBaseOutcome {
    /// The base is already an ancestor of HEAD; nothing to merge.
    UpToDate,
    /// Clean merge; `sha` is the new merge commit (HEAD).
    Merged { sha: String },
    /// The merge hit conflicts and was left in progress for resolution;
    /// `files` are the unmerged paths.
    Conflicts { files: Vec<String> },
    /// An in-progress merge was rolled back (or none existed).
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCheckoutReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    pub branch: String,
    #[serde(default)]
    pub create: bool,
}

impl WorkspaceRpc for GitCheckoutReq {
    const METHOD: &'static str = "workspace.git_checkout";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = ();
}

// Explicit-git-surface ops (control plane → sandbox). Git mutates only through
// these platform ops; there is no autonomous agent commit/push/merge.

/// `EnsureBinding` — ensure the conversation branch (`conv/<id>`) exists and is
/// checked out, forking it off `base_ref` if absent (never writing the base).
/// Idempotent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitEnsureBindingReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    /// The conversation branch to ensure, e.g. `conv/<conversation_id>`.
    pub session_branch: String,
    /// Ref to fork the conv branch from when it does not yet exist locally: the
    /// remote default branch, a branch tip, or a commit SHA.
    pub base_ref: String,
}

impl WorkspaceRpc for GitEnsureBindingReq {
    const METHOD: &'static str = "workspace.git_ensure_binding";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = GitEnsureBindingResult;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitEnsureBindingResult {
    /// The branch now checked out (echoes `session_branch`).
    pub branch: String,
    /// `true` if the conv branch was created off `base_ref` by this call.
    pub created: bool,
    /// HEAD after the op (full hex), if the repo has any commits.
    pub head_sha: Option<String>,
}

/// `MergeToMain` — merge the conversation branch into its target branch (the
/// publish path, or an explicit "Merge" button). Merge, never rebase; never
/// force. On conflicts the implementer aborts and restores `session_branch`
/// (no `MERGE_HEAD` left on the integration branch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitMergeToMainReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    /// The conversation branch to merge (`conv/<id>`). Same identity as
    /// [`super::super::binding::ResolvedRepoSource::session_branch`].
    pub session_branch: String,
    /// Integration branch from the binding resolver
    /// (`ResolvedRepoSource.merge_target`). `None` or empty is a hard error —
    /// do not serde-default to `main` (BYO remotes use `master`/`trunk`).
    #[serde(default)]
    pub target_branch: Option<String>,
    /// Push the target branch after a successful merge.
    #[serde(default)]
    pub push: bool,
}

impl GitMergeToMainReq {
    /// Resolved non-empty target, or `None` if the caller omitted/blanked it.
    pub fn required_target_branch(&self) -> Option<&str> {
        self.target_branch
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

impl WorkspaceRpc for GitMergeToMainReq {
    const METHOD: &'static str = "workspace.git_merge_to_main";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = GitMergeToMainResult;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitMergeToMainResult {
    pub outcome: GitMergeToMainOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitMergeToMainOutcome {
    /// The conv branch is already an ancestor of the target; nothing to merge.
    /// `sha` is the target HEAD.
    UpToDate { sha: String },
    /// Clean merge (or fast-forward); `sha` is the new target HEAD.
    Merged { sha: String },
    /// The merge hit conflicts; the implementer aborted and restored
    /// `session_branch`. `files` are the unmerged paths. The workspace must
    /// not be left with `MERGE_HEAD` on the integration branch.
    Conflicts { files: Vec<String> },
}

/// `Push` — push a branch to `origin` after a commit, classifying the outcome.
/// Never forces (a non-fast-forward on a single-writer conv branch is a
/// conflict to surface, not to overwrite).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitPushReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    /// Branch to push. `None` ⇒ current `HEAD`.
    #[serde(default)]
    pub branch: Option<String>,
}

impl WorkspaceRpc for GitPushReq {
    const METHOD: &'static str = "workspace.git_push";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = GitPushResult;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitPushResult {
    pub status: PushStatus,
    /// Combined git output (sanitized), for diagnostics/FE.
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStashReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
    #[serde(default)]
    pub include_untracked: bool,
}

impl WorkspaceRpc for GitStashReq {
    const METHOD: &'static str = "workspace.git_stash";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = ();
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitInfoReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
}

impl WorkspaceRpc for GitInfoReq {
    const METHOD: &'static str = "workspace.git_info";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = GitInfoData;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitBranchesReq {
    #[serde(default)]
    pub git_root: Option<std::path::PathBuf>,
}

impl WorkspaceRpc for GitBranchesReq {
    const METHOD: &'static str = "workspace.git_branches";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = GitBranchListData;
}

/// Resolve the git root from a path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitResolveRootReq {
    pub cwd: std::path::PathBuf,
}

impl WorkspaceRpc for GitResolveRootReq {
    const METHOD: &'static str = "workspace.git_resolve_root";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Option<std::path::PathBuf>;
}

/// Get the current commit hash for a git root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCurrentCommitReq {
    pub git_root: std::path::PathBuf,
}

impl WorkspaceRpc for GitCurrentCommitReq {
    const METHOD: &'static str = "workspace.git_current_commit";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Option<String>;
}

/// Detect VCS kind (git vs jj) for a path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectVcsKindReq {
    pub path: std::path::PathBuf,
}

impl WorkspaceRpc for DetectVcsKindReq {
    const METHOD: &'static str = "workspace.detect_vcs_kind";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = VcsKind;
}

/// Checkout a specific commit with optional auto-stash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCheckoutCommitReq {
    pub git_root: std::path::PathBuf,
    pub head_commit: String,
    pub head_branch: Option<String>,
    pub stash_if_dirty: bool,
}

impl WorkspaceRpc for GitCheckoutCommitReq {
    const METHOD: &'static str = "workspace.git_checkout_commit";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = CheckoutCommitResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutCommitResponse {
    pub checked_out: bool,
    pub stashed: bool,
    pub fetched: bool,
    pub error: Option<String>,
}

/// `workspace.git_branch_info`. The server returns the [`GitInfoData`]
/// object, or `null` when the workspace root is not a git repo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitBranchInfoReq {}

impl WorkspaceRpc for GitBranchInfoReq {
    const METHOD: &'static str = "workspace.git_branch_info";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Option<GitInfoData>;
}

/// `workspace.git_metadata`. The response value is the persisted session
/// git metadata object (or `null`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitMetadataReq {}

impl WorkspaceRpc for GitMetadataReq {
    const METHOD: &'static str = "workspace.git_metadata";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

// ---- Serde helpers ------------------------------------------------------

fn default_true() -> bool {
    true
}
fn default_head() -> String {
    "HEAD".into()
}
fn default_working() -> String {
    "working".into()
}
fn default_max_file_bytes() -> u64 {
    0 // No limit by default
}

// =========================================================================
// Response data types
// =========================================================================

/// The kind of version control system detected for a workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VcsKind {
    /// Pure git repository (`.git/` only).
    #[default]
    Git,
    /// Jujutsu colocated with git (`.jj/` + `.git/`).
    JujutsuColocated,
    /// No VCS detected.
    None,
}

impl VcsKind {
    /// Returns `true` if this is a Jujutsu-managed repo (colocated).
    pub fn is_jj(&self) -> bool {
        matches!(self, VcsKind::JujutsuColocated)
    }

    /// Returns `true` if any VCS was detected.
    pub fn is_repo(&self) -> bool {
        !matches!(self, VcsKind::None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Create,
    Edit,
    Delete,
    Rename,
    Copy,
    Typechange,
    /// Untracked file (not yet added to git)
    Untracked,
}

/// Selector for the `git_status_ext` output shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitStatusFormat {
    /// Return structured GitStatusData (default).
    #[default]
    Structured,
    /// Return compact plain-text status suitable for prompt injection.
    Prompt,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CommitResult {
    pub data: CommitData,
    pub warning: Option<String>,
    /// Structured outcome of the commit + push, for machine callers.
    /// `None` from servers predating the field and from the jj backend;
    /// `data`/`warning` are the human-readable channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CommitOutcome>,
}

/// Machine-readable result of a `workspace.git_commit` call.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitOutcome {
    /// Workspace HEAD after the op (full hex). `None` only when the repo has
    /// no commits at all.
    pub sha: Option<String>,
    /// Nothing new was committed by this call (tree clean after staging).
    pub clean: bool,
    /// The push (when requested) delivered the branch to the remote.
    pub pushed: bool,
    pub push: PushStatus,
}

/// Push-step classification for [`CommitOutcome`]. A non-fast-forward
/// rejection on a single-writer conversation branch means the remote
/// diverged and needs resolution — the op never forces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushStatus {
    /// No push was requested.
    #[default]
    NotRequested,
    /// A push was requested but skipped because a prior step (the `sync`
    /// pull) failed; nothing was pushed.
    Skipped,
    Ok,
    /// Rejected as non-fast-forward (remote diverged); never forced.
    Conflict,
    /// Failed for any other reason (auth, network, remote unavailable).
    Failed,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageData {
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(rename = "type")]
    pub change_type: ChangeType,
    /// Whether this change is staged. None when not applicable (e.g., diffs between commits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged: Option<bool>,
    pub additions: u64,
    pub deletions: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_lines: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_worktree: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Commits ahead of upstream (local commits not pushed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<usize>,
    /// Commits behind upstream (remote commits not pulled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<usize>,
    pub staged: Vec<GitFileChange>,
    pub unstaged: Vec<GitFileChange>,
}

/// Response wrapper for `git_status_ext` that always has the same shape regardless of format.
///
/// This avoids the deserialization ambiguity of an untagged enum by using
/// a tagged struct with optional fields. Callers check `format` to know
/// which field to use.
///
/// `Deserialize` is implemented manually (see below) so that a legacy flat
/// `GitStatusData` payload — returned by an older workspace server during a
/// version skew — is recognized and wrapped as `format: Structured` rather than
/// silently parsed as empty.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GitStatusExtResponse {
    /// The format of this response (echoed from request for convenience).
    pub format: GitStatusFormat,

    /// Structured status data (present when format = "structured").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<GitStatusData>,

    /// Prompt-formatted string (present when format = "prompt").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

impl<'de> Deserialize<'de> for GitStatusExtResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize into a generic value first so we can distinguish the new
        // envelope from a legacy flat `GitStatusData` payload (version skew with
        // an older workspace server that still returns flat status for
        // `git_status_ext`).
        let value = Value::deserialize(deserializer)?;

        // The new envelope is identified by any of its own keys; `format` is
        // always serialized, and `data`/`prompt` cover any hand-written payload.
        // A legacy flat `GitStatusData` (root/branch/staged/...) has none of them.
        let is_new_envelope = value.as_object().is_some_and(|obj| {
            obj.contains_key("format") || obj.contains_key("data") || obj.contains_key("prompt")
        });

        if is_new_envelope {
            #[derive(Deserialize)]
            struct Envelope {
                #[serde(default)]
                format: GitStatusFormat,
                #[serde(default)]
                data: Option<GitStatusData>,
                #[serde(default)]
                prompt: Option<String>,
            }
            let env: Envelope = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(GitStatusExtResponse {
                format: env.format,
                data: env.data,
                prompt: env.prompt,
            })
        } else {
            // Legacy flat payload: parse as `GitStatusData` and wrap it.
            let data: GitStatusData =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(GitStatusExtResponse {
                format: GitStatusFormat::Structured,
                data: Some(data),
                prompt: None,
            })
        }
    }
}

impl GitStatusExtResponse {
    /// Create a structured response.
    pub fn structured(data: GitStatusData) -> Self {
        Self {
            format: GitStatusFormat::Structured,
            data: Some(data),
            prompt: None,
        }
    }

    /// Create a prompt-formatted response.
    pub fn prompt(text: String) -> Self {
        Self {
            format: GitStatusFormat::Prompt,
            data: None,
            prompt: Some(text),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitError {
    pub path: Option<String>,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReadFilesData {
    pub files: Vec<GitReadFile>,
    pub errors: Vec<GitError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitReadFile {
    pub path: String,
    pub version: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_binary: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffsData {
    pub files: Vec<GitFileChange>,
}

impl GitDiffsData {
    pub fn collect_patches(&self) -> Option<String> {
        let patches: Vec<_> = self
            .files
            .iter()
            .filter_map(|f| f.patch.as_deref())
            .collect();
        (!patches.is_empty()).then(|| patches.join("\n"))
    }
}

/// Scope for discard operations
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscardScope {
    Working,
    Staged,
    #[default]
    Both,
}

/// Structured repo info returned by `echo.agent/git/info`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInfoData {
    /// Absolute path to the repo root.
    pub root: String,
    /// Unique remote URLs (sorted).
    pub remotes: Vec<String>,
    /// Current branch name (`None` for detached HEAD).
    pub current_branch: Option<String>,
    /// Default branch of the primary remote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// The detected VCS kind. Desktop uses this to disable features for unsupported VCS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs_kind: Option<VcsKind>,
}

/// Single branch entry returned by `echo.agent/git/branches`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchEntry {
    pub name: String,
    pub current: bool,
    pub remote: bool,
}

/// Structured result of `echo.agent/git/branches`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchListData {
    pub current_branch: Option<String>,
    pub repo_root: String,
    pub branches: Vec<GitBranchEntry>,
}

// =========================================================================
// Git Collect Changes RPC Types
// =========================================================================

/// Request to collect repository changes for serialization.
/// This is the workspace-side half of `serialize_changes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCollectChangesReq {
    /// Any path inside the repo/worktree. The git root is discovered from this path.
    /// Named `repo_path` (not `git_root`) to match `SerializeRepoChangesRequest`.
    pub repo_path: String,

    /// Include commit series (typically commits ahead of upstream).
    #[serde(default = "default_true")]
    pub include_commits: bool,

    /// Include uncommitted changes (staged/unstaged/untracked).
    #[serde(default = "default_true")]
    pub include_uncommitted: bool,

    /// Optional public base revision override (like `origin/main` or a commit SHA).
    /// If omitted, auto-detects the latest public commit on HEAD's first-parent history.
    #[serde(default)]
    pub base_ref: Option<String>,

    /// Maximum bytes to inline for a single file blob in commit/uncommitted
    /// patches. `0` (default) means no limit; larger blobs are truncated with a
    /// warning. Untracked file content is governed separately by the fixed
    /// [`UNTRACKED_CONTENT_THRESHOLD`]: oversize untracked files are excluded
    /// (not truncated) rather than capped by this value.
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,

    /// Absolute paths captured even when gitignored.
    #[serde(default)]
    pub force_include_paths: Vec<std::path::PathBuf>,
}

impl WorkspaceRpc for GitCollectChangesReq {
    const METHOD: &'static str = "workspace.git_collect_changes";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = GitCollectChangesResponse;
}

/// Response containing collected repository changes as serializable wire types.
/// The conversion is lossless for all fields needed by the shell to build the
/// archive and upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCollectChangesResponse {
    /// Repository metadata (root, branch, remotes, ahead/behind, etc.).
    pub repo: RepoInfo,

    /// HEAD commit OID (hex string).
    pub head: String,

    /// Public base commit (the merge-base with remote default branch).
    pub public_base: PublicBaseData,

    /// Commits ahead of base (with patches).
    pub commits: Vec<CommitWithPatchData>,

    /// Uncommitted changes (staged + unstaged).
    pub uncommitted: Option<UncommittedChangesData>,

    /// Untracked files with content (base64-encoded if included).
    pub untracked: Vec<UntrackedFileData>,

    /// Warnings (e.g., files skipped due to size limits, binary files excluded).
    pub warnings: Vec<String>,

    /// Total size of collected data (bytes, for logging/telemetry).
    pub total_size_bytes: u64,
}

/// Repository info for wire transfer.
///
/// Contains metadata about the repository including root path, branch,
/// remotes, and ahead/behind counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    /// Absolute path to the repo root.
    pub root: String,
    /// Path to the .git directory (may differ for worktrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_dir: Option<String>,
    /// Current HEAD commit OID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Current branch name (None for detached HEAD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Whether HEAD is detached.
    #[serde(default)]
    pub is_detached: bool,
    /// Upstream branch (e.g., "origin/main").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Upstream HEAD commit OID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_head: Option<String>,
    /// Remote URL (first remote's URL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Commits ahead of upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<usize>,
    /// Commits behind upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<usize>,
}

/// Diff stats summary for wire transfer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStatsSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// Public base commit info for wire transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicBaseData {
    /// The public base commit OID (hex string).
    pub commit: String,
    /// Remote-tracking refs that contain this commit.
    pub refs: Vec<String>,
}

/// Commit data for wire transfer.
///
/// Serializable fields; the `patch` is base64-encoded for binary safety.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitWithPatchData {
    /// Commit OID (hex string).
    pub id: String,
    /// Parent commit OIDs (hex strings).
    pub parents: Vec<String>,
    /// Author identity.
    pub author: IdentityData,
    /// Committer identity.
    pub committer: IdentityData,
    /// First line of commit message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Full commit message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Patch content (base64-encoded). None if patch was too large.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_base64: Option<String>,
    /// Diff stats for this commit.
    pub stats: DiffStatsSummary,
    /// Binary files that changed in this commit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binary_files: Vec<BinaryFileInfoData>,
}

/// Author/committer identity for wire transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// RFC3339 timestamp in the author/committer timezone (if representable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// Unix timestamp in seconds.
    pub time_seconds: i64,
    /// Timezone offset in minutes from UTC.
    pub offset_minutes: i32,
}

/// Binary file info for wire transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BinaryFileInfoData {
    /// File path relative to repo root.
    pub path: String,
    /// Change status (e.g., "added", "modified", "deleted").
    pub status: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Whether the blob content is included.
    pub blob_included: bool,
    /// Whether the content was truncated.
    pub truncated: bool,
    /// Reason for exclusion (if not included).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_reason: Option<String>,
    /// Blob content (base64-encoded). None if not included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
}

/// Uncommitted changes for wire transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UncommittedChangesData {
    /// Staged patch (base64-encoded). None if no staged changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_patch_base64: Option<String>,
    /// Diff stats for staged changes.
    pub staged_stats: DiffStatsSummary,
    /// Unstaged patch (base64-encoded). None if no unstaged changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unstaged_patch_base64: Option<String>,
    /// Diff stats for unstaged changes.
    pub unstaged_stats: DiffStatsSummary,
    /// Binary files in staged changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub staged_binary_files: Vec<BinaryFileInfoData>,
    /// Binary files in unstaged changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unstaged_binary_files: Vec<BinaryFileInfoData>,
}

/// Untracked file info for wire transfer.
///
/// Content inclusion rules:
/// - Files larger than [`UNTRACKED_CONTENT_THRESHOLD`] (1 MB) have
///   `content_base64: None` and `content_included: false`.
/// - Binary files (`is_binary: true`) have `content_base64: None` regardless of size.
/// - Omitted content can be fetched via `workspace.fs_read_file`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UntrackedFileData {
    /// File path relative to repo root.
    pub path: String,
    /// Whether the file is binary.
    pub is_binary: bool,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Whether the content was truncated.
    pub truncated: bool,
    /// File content (base64-encoded). None if file exceeds threshold or is binary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    /// Whether content was included in the response.
    pub content_included: bool,
}

/// Threshold for including untracked file content in the RPC response (1 MB).
/// Files larger than this have `content_base64: None` and must be fetched separately.
pub const UNTRACKED_CONTENT_THRESHOLD: u64 = 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_status_ext_req_omitted_include_untracked_is_false() {
        let from_json: GitStatusExtReq = serde_json::from_str("{}").unwrap();
        assert!(!from_json.include_untracked);
        assert!(from_json.ignore_submodules);
        assert!(!GitStatusExtReq::default().include_untracked);
        assert!(GitStatusExtReq::default().ignore_submodules);
    }

    #[test]
    fn method_constant() {
        assert_eq!(GitStatusReq::METHOD, "workspace.git_status");
        assert_eq!(GitStatusExtReq::METHOD, "workspace.git_status_ext");
        assert_eq!(GitBranchInfoReq::METHOD, "workspace.git_branch_info");
        assert_eq!(GitMetadataReq::METHOD, "workspace.git_metadata");
        assert_eq!(
            GitCollectChangesReq::METHOD,
            "workspace.git_collect_changes"
        );
    }

    #[test]
    fn git_file_change_serializes_type_key() {
        let change = GitFileChange {
            path: "src/main.rs".into(),
            old_path: None,
            change_type: ChangeType::Edit,
            staged: Some(true),
            additions: 1,
            deletions: 2,
            patch: None,
            patch_bytes: None,
            patch_lines: None,
            old_text: None,
            new_text: None,
        };
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["type"], "edit");
        assert!(json.get("oldPath").is_none(), "camelCase + skip none");
    }

    #[test]
    fn vcs_kind_camel_case_wire_values() {
        assert_eq!(
            serde_json::to_value(VcsKind::JujutsuColocated).unwrap(),
            serde_json::json!("jujutsuColocated")
        );
        assert_eq!(
            serde_json::to_value(VcsKind::Git).unwrap(),
            serde_json::json!("git")
        );
    }

    #[test]
    fn git_status_format_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(GitStatusFormat::Structured).unwrap(),
            serde_json::json!("structured")
        );
        assert_eq!(
            serde_json::to_value(GitStatusFormat::Prompt).unwrap(),
            serde_json::json!("prompt")
        );
    }

    #[test]
    fn git_status_format_deserializes_lowercase() {
        let structured: GitStatusFormat =
            serde_json::from_value(serde_json::json!("structured")).unwrap();
        let prompt: GitStatusFormat = serde_json::from_value(serde_json::json!("prompt")).unwrap();
        assert_eq!(structured, GitStatusFormat::Structured);
        assert_eq!(prompt, GitStatusFormat::Prompt);
    }

    #[test]
    fn git_status_ext_response_structured_constructor() {
        let data = GitStatusData::default();
        let response = GitStatusExtResponse::structured(data.clone());

        assert_eq!(response.format, GitStatusFormat::Structured);
        assert!(response.data.is_some());
        assert!(response.prompt.is_none());

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["format"], "structured");
        assert!(json.get("data").is_some());
        assert!(
            json.get("prompt").is_none(),
            "prompt should be skipped when None"
        );
    }

    #[test]
    fn git_status_ext_response_prompt_constructor() {
        let response = GitStatusExtResponse::prompt("On branch main".to_string());

        assert_eq!(response.format, GitStatusFormat::Prompt);
        assert!(response.data.is_none());
        assert_eq!(response.prompt, Some("On branch main".to_string()));

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["format"], "prompt");
        assert!(
            json.get("data").is_none(),
            "data should be skipped when None"
        );
        assert_eq!(json["prompt"], "On branch main");
    }

    #[test]
    fn git_status_ext_response_roundtrip_structured() {
        let data = GitStatusData {
            branch: Some("main".to_string()),
            ahead: Some(1),
            behind: Some(0),
            ..Default::default()
        };
        let original = GitStatusExtResponse::structured(data);
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: GitStatusExtResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.format, GitStatusFormat::Structured);
        assert!(deserialized.data.is_some());
        let data = deserialized.data.unwrap();
        assert_eq!(data.branch, Some("main".to_string()));
        assert_eq!(data.ahead, Some(1));
        assert_eq!(data.behind, Some(0));
    }

    #[test]
    fn git_status_ext_response_roundtrip_prompt() {
        let original = GitStatusExtResponse::prompt("Changes not staged for commit".to_string());
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: GitStatusExtResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.format, GitStatusFormat::Prompt);
        assert!(deserialized.data.is_none());
        assert_eq!(
            deserialized.prompt,
            Some("Changes not staged for commit".to_string())
        );
    }

    #[test]
    fn git_status_ext_response_default() {
        let response = GitStatusExtResponse::default();
        assert_eq!(response.format, GitStatusFormat::Structured);
        assert!(response.data.is_none());
        assert!(response.prompt.is_none());
    }

    #[test]
    fn git_status_ext_response_deserializes_new_structured_envelope() {
        let json = serde_json::json!({
            "format": "structured",
            "data": { "branch": "main", "staged": [], "unstaged": [] },
        });
        let resp: GitStatusExtResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.format, GitStatusFormat::Structured);
        assert_eq!(resp.data.unwrap().branch, Some("main".to_string()));
        assert!(resp.prompt.is_none());
    }

    #[test]
    fn git_status_ext_response_deserializes_new_prompt_envelope() {
        let json = serde_json::json!({
            "format": "prompt",
            "prompt": "On branch main",
        });
        let resp: GitStatusExtResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.format, GitStatusFormat::Prompt);
        assert!(resp.data.is_none());
        assert_eq!(resp.prompt, Some("On branch main".to_string()));
    }

    #[test]
    fn git_status_ext_response_deserializes_legacy_flat_status() {
        // A legacy workspace server returns flat `GitStatusData` JSON for
        // `git_status_ext`; it must be wrapped as a structured envelope rather
        // than parsed as an empty response.
        let legacy = serde_json::to_value(GitStatusData {
            branch: Some("main".to_string()),
            ahead: Some(2),
            behind: Some(1),
            staged: vec![],
            unstaged: vec![],
            ..Default::default()
        })
        .unwrap();
        // Sanity: the legacy payload has none of the envelope's own keys.
        assert!(legacy.get("format").is_none());
        assert!(legacy.get("data").is_none());
        assert!(legacy.get("prompt").is_none());

        let resp: GitStatusExtResponse = serde_json::from_value(legacy).unwrap();
        assert_eq!(resp.format, GitStatusFormat::Structured);
        assert!(resp.prompt.is_none());
        let data = resp
            .data
            .expect("legacy flat status should be wrapped into data");
        assert_eq!(data.branch, Some("main".to_string()));
        assert_eq!(data.ahead, Some(2));
        assert_eq!(data.behind, Some(1));
    }
}
