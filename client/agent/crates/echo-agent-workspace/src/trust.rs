//! Folder-trust store ("do you trust this folder?").
//!
//! Persists per-folder trust decisions to `~/.echo-agent/trusted_folders.toml`.
//! This is the durable backing store for the VS-Code-style folder-trust gate
//! that decides whether repo-local MCP / LSP servers (which run arbitrary
//! commands from repo-controlled config files) are allowed to spawn.
//!
//! TOML shape:
//! ```toml
//! [folders."/abs/repo/root"]
//! trusted = true
//! decided_at = 1780000000
//! ```
//!
//! Trust **cascades to subdirectories**: if a folder is recorded trusted, any
//! path at or below it is considered trusted, unless a nearer (more specific)
//! folder records its own decision — the longest matching path prefix wins, so
//! an explicit child untrust overrides an ancestor's trust. The persisted file
//! is written atomically with owner-only (`0600`) permissions.
//!
//! The store is rooted at [`echo_agent_config::user_echo_agent_home`] — the **Option**
//! home that resolves to `None` (rather than a cwd-relative `./.echo-agent`) when
//! neither `$ECHO_AGENT_HOME` nor a home directory is set (e.g. a minimal container /
//! CI). In that no-home environment [`TrustStore::load`] yields an **empty,
//! trust-nothing** store that persists nothing, so a cloned repo can never ship
//! a `./.echo-agent/trusted_folders.toml` that self-trusts its own checkout (fail
//! closed).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Filename of the folder-trust store under `~/.echo-agent/`.
pub const TRUST_FILE_NAME: &str = "trusted_folders.toml";

/// A single folder's trust record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderTrust {
    /// Whether the folder (and its subdirectories) is trusted.
    pub trusted: bool,
    /// Unix timestamp (seconds) of when the decision was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
}

/// On-disk document shape for `trusted_folders.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustDocument {
    #[serde(default)]
    folders: BTreeMap<String, FolderTrust>,
}

/// Persisted set of trusted folders.
///
/// Construct with [`TrustStore::load`] (production) or [`TrustStore::load_from`]
/// (tests). Mutating with [`TrustStore::set_trusted`] persists to disk.
///
/// `path` is `None` only in a no-home environment (see [`TrustStore::load`]):
/// such a store holds no folders, trusts nothing, and persists nothing.
#[derive(Debug, Clone)]
pub struct TrustStore {
    doc: TrustDocument,
    /// Backing file, or `None` when no user home resolves — a trust-nothing,
    /// persist-nothing store. Never a cwd-relative path.
    path: Option<PathBuf>,
}

impl TrustStore {
    /// Load the trust store from `<user_echo_agent_home>/trusted_folders.toml`.
    ///
    /// When no user home resolves (see the module-level fail-closed note) the
    /// path is `None` and this returns an [`Self::empty`] store. Otherwise an
    /// empty store is returned if the file is missing or unparseable (logged).
    pub fn load() -> Self {
        match Self::default_path() {
            Some(path) => Self::load_from(path),
            None => Self::empty(),
        }
    }

    /// Load from a custom path (for tests).
    pub fn load_from(path: PathBuf) -> Self {
        let doc = Self::read_doc(&path);
        Self {
            doc,
            path: Some(path),
        }
    }

    /// An empty store with no backing path: trusts nothing and persists
    /// nothing. Used for the no-home environment where [`Self::default_path`]
    /// resolves to `None`.
    fn empty() -> Self {
        Self {
            doc: TrustDocument::default(),
            path: None,
        }
    }

    /// Default on-disk path: `<user_echo_agent_home>/trusted_folders.toml`, or `None`
    /// when no user home resolves.
    ///
    /// Resolves via [`echo_agent_config::user_echo_agent_home`], never
    /// [`echo_agent_config::echo_agent_home`], so it never falls back to a cwd-relative
    /// `./.echo-agent` — that fallback would let an untrusted cloned repo's `.echo-agent`
    /// masquerade as the user-global store and self-trust the checkout.
    pub fn default_path() -> Option<PathBuf> {
        Self::default_path_in(echo_agent_config::user_echo_agent_home())
    }

    /// Map a resolved user-echo-agent-home to the store path, preserving "no home" as
    /// "no path" (never synthesizing a fallback). Split from [`Self::default_path`]
    /// as a pure seam so the no-home branch is unit-testable without the
    /// process-global home cache.
    fn default_path_in(user_echo_agent_home: Option<PathBuf>) -> Option<PathBuf> {
        Some(user_echo_agent_home?.join(TRUST_FILE_NAME))
    }

    /// Whether `workspace_key` is trusted, per the MOST-SPECIFIC recorded
    /// decision (longest matching path prefix).
    ///
    /// This is the SHARED folder-trust gate: the most-specific-wins semantics
    /// apply to ALL folder-trust surfaces (repo-local MCP and LSP servers, and
    /// project hooks), not just hooks.
    ///
    /// Trust cascades to subdirectories: a trusted parent folder trusts all of
    /// its children. When both an ancestor and a nearer folder are recorded, the
    /// longest matching prefix wins, so an explicit child untrust overrides an
    /// ancestor's trust instead of being undone by the cascade. The query key is
    /// canonicalized here, so callers need not pre-canonicalize (symmetric with
    /// [`Self::set_trusted`]).
    ///
    /// Over-broad keys are ignored on read (fail closed): an empty/relative
    /// key, the filesystem root, or the user's home directory are never honored
    /// even if such a record reaches the file via hand-edit or migration — each
    /// would otherwise trust huge swaths of the filesystem through the cascade.
    /// See [`is_unsafe_trust_root`].
    pub fn is_trusted(&self, workspace_key: &Path) -> bool {
        let workspace_key = canonicalize_or_owned(workspace_key);
        // Among all recorded ancestor folders (including the key itself), the
        // longest match decides. Canonical, code-produced keys are normalized, so
        // that longest match is unique. A hand-edited store could hold
        // non-canonical aliases (e.g. `/a/b` vs `/a/b/`) that tie on depth; on a
        // tie we require EVERY tied record to be trusted, so a contradictory edit
        // fails closed.
        let mut best_depth: Option<usize> = None;
        let mut trusted = false;
        for (folder, record) in &self.doc.folders {
            let folder = Path::new(folder);
            if is_unsafe_trust_root(folder) || !workspace_key.starts_with(folder) {
                continue;
            }
            let depth = folder.components().count();
            match best_depth {
                Some(d) if depth < d => {}
                Some(d) if depth == d => trusted &= record.trusted,
                _ => {
                    best_depth = Some(depth);
                    trusted = record.trusted;
                }
            }
        }
        trusted
    }

    /// Record `workspace_key` as **trusted** and persist to disk.
    ///
    /// The key is canonicalized before storage so alias spellings (symlinks,
    /// `/tmp` vs `/private/tmp`, …) still match later lookups, which canonicalize
    /// too. Keys are stored as UTF-8 strings (via `to_string_lossy`); a non-UTF-8
    /// path (rare on Unix) is stored lossily and therefore fails closed (it
    /// simply won't match on lookup) rather than over-trusting.
    ///
    /// **Over-broad roots are refused:** if the canonical key is non-absolute,
    /// the filesystem root, or the user's home directory it is rejected —
    /// nothing is recorded (neither in memory nor on disk) and `Ok(())` is
    /// returned, so `is_trusted` stays `false` for it on both read and write.
    /// A later [`Self::set_untrusted`] on the same folder flips the stored
    /// decision (the insert overwrites). See `record_decision` for the locked
    /// read-modify-write contract and the no-home `Ok(())` no-op.
    pub fn set_trusted(&mut self, workspace_key: &Path) -> io::Result<()> {
        self.record_decision(workspace_key, true)
    }

    /// Record `workspace_key` as **untrusted** ("Never" / explicitly declined)
    /// and persist to disk.
    ///
    /// Mirrors [`Self::set_trusted`] exactly (canonicalization + over-broad-root
    /// refusal) but stores `trusted = false`. [`Self::is_trusted`] already
    /// returns `false` for such a record; recording it lets a consumer tell
    /// "explicitly declined" apart from "undecided" (e.g. to avoid
    /// re-prompting). A later [`Self::set_trusted`] flips it back.
    pub fn set_untrusted(&mut self, workspace_key: &Path) -> io::Result<()> {
        self.record_decision(workspace_key, false)
    }

    /// Number of recorded folders (for diagnostics / tests).
    pub fn len(&self) -> usize {
        self.doc.folders.len()
    }

    /// Whether the store has no recorded folders.
    pub fn is_empty(&self) -> bool {
        self.doc.folders.is_empty()
    }

    /// Whether `workspace_key` has an EXACT recorded decision (trusted OR
    /// untrusted) — not cascade-aware. Used by the legacy-hook-trust migration to
    /// avoid overriding a folder the user has already decided on.
    pub fn has_decision(&self, workspace_key: &Path) -> bool {
        let canonical = canonicalize_or_owned(workspace_key);
        self.doc
            .folders
            .contains_key(canonical.to_string_lossy().as_ref())
    }

    // ── Internal ──────────────────────────────────────────────────────

    /// Shared write path for [`Self::set_trusted`] / [`Self::set_untrusted`].
    ///
    /// Canonicalizes the key, refuses over-broad roots (non-absolute /
    /// filesystem root / home dir → `warn!` + `Ok(())` recording nothing), and
    /// is a `warn!` + `Ok(())` no-op when there is no backing path (no-home
    /// environment), so it never writes a cwd-relative file. Otherwise it
    /// performs a locked read-modify-write-commit:
    /// 1. take an exclusive advisory lock on a sidecar `*.toml.lock` file, held
    ///    for the whole critical section (released on drop), so concurrent
    ///    writers serialize;
    /// 2. re-read the current on-disk document so a peer's decisions are merged
    ///    rather than clobbered (lost-update fix);
    /// 3. insert the record and persist atomically;
    /// 4. only on success commit the new document to memory — on any
    ///    lock/persist error `self.doc` is left unchanged.
    fn record_decision(&mut self, workspace_key: &Path, trusted: bool) -> io::Result<()> {
        let canonical = canonicalize_or_owned(workspace_key);
        if is_unsafe_trust_root(&canonical) {
            tracing::warn!(
                path = %canonical.display(),
                trusted,
                "folder trust: refusing to record an over-broad root (home, filesystem root, or non-absolute path); nothing recorded"
            );
            return Ok(());
        }

        // No backing file (no-home env) → record nothing, return `Ok` so
        // callers treat "no home" like "nothing to persist" (see fn doc).
        let Some(path) = self.path.as_deref() else {
            tracing::warn!(
                path = %canonical.display(),
                trusted,
                "folder trust: no user echoagent home resolved; trust decision not recorded"
            );
            return Ok(());
        };

        // The lock file lives beside the store, so ensure the dir exists first.
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust store path has no parent",
            )
        })?;
        std::fs::create_dir_all(parent)?;

        // Serialize cross-process writers for the whole read-modify-write so a
        // concurrent peer's records are preserved, not clobbered.
        let _lock = ExclusiveLock::acquire(&path.with_extension("toml.lock"))?;

        // Re-read the latest on-disk state (merges a peer's concurrent writes).
        let mut doc = Self::read_doc(path);
        doc.folders.insert(
            canonical.to_string_lossy().to_string(),
            FolderTrust {
                trusted,
                decided_at: now_unix(),
            },
        );

        // Commit to memory only after a successful durable write, so a failure
        // leaves the in-memory store unchanged.
        Self::persist_doc(path, &doc)?;
        self.doc = doc;
        Ok(())
    }

    fn read_doc(path: &Path) -> TrustDocument {
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return TrustDocument::default(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "folder trust: failed to read trust store; treating as empty"
                );
                return TrustDocument::default();
            }
        };
        toml::from_str(&contents).unwrap_or_else(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "folder trust: failed to parse trust store; treating as empty"
            );
            TrustDocument::default()
        })
    }

    /// Write `doc` to `path` atomically (unique temp + fsync + rename) with
    /// owner-only (`0600`) permissions.
    ///
    /// Uses a unique temp file in the destination directory so concurrent
    /// writers never share a temp path, fsyncs it for crash durability, then
    /// renames it over the destination. `tempfile::NamedTempFile` creates the
    /// temp with `O_EXCL` and `0600` permissions on Unix, and `persist`
    /// performs an atomic replace (including over an existing destination on
    /// Windows).
    fn persist_doc(path: &Path, doc: &TrustDocument) -> io::Result<()> {
        use std::io::Write;

        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust store path has no parent",
            )
        })?;
        std::fs::create_dir_all(parent)?;

        let body = toml::to_string_pretty(doc)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Unique temp in the same directory (atomic rename requires same FS).
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.write_all(body.as_bytes())?;
        // Durably flush to disk before publishing so a crash can't leave a
        // zero-length or stale store behind. (`File::flush` is a no-op for
        // durability; `sync_all` is what guarantees the bytes hit disk.)
        tmp.as_file().sync_all()?;
        // Atomic publish.
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

/// Compute the trust **workspace key** for a working directory.
///
/// The key is the canonicalized git repository root when `cwd` is inside a
/// repo (trust applies to the whole repo), otherwise the canonicalized `cwd`.
///
/// A echo-agent-managed worktree first collapses onto its recorded source repo's git
/// ROOT (via the `~/.echo-agent/worktrees.db` registry), so every `echoagent -w` worktree
/// shares one trust key regardless of creation mode — including standalone clones
/// that git can't link back to their source — and regardless of the subdir
/// `echoagent -w` was launched from (the recorded source repo may be a repo subdir).
/// Non-registry git worktrees fall through to the git-topology collapse below.
///
/// A linked git worktree collapses onto its MAIN checkout's root so every
/// `echoagent -w` worktree of a repo shares one trust key. The collapse fires ONLY
/// for the conventional `<workdir>/.git` layout — i.e. the common gitdir
/// resolves back to `<main_workdir>/.git`. For bare or `--separate-git-dir`
/// repos (where the common gitdir's inferred workdir would be the gitdir's
/// parent, broader than the real checkout) the key instead falls back to the
/// worktree's own workdir, so it is narrow and never widened. Resolution is via
/// git2 (honoring `core.worktree`), never path surgery.
///
/// Finally, an over-broad derived root is rejected in favor of the cwd: when
/// `$HOME` is itself a git repo (dotfiles-in-home) the up-walk would otherwise
/// land on the home dir, so [`is_unsafe_trust_root`] re-scopes the key to the
/// cwd (keeping trust bound to the working dir, not the whole home subtree). A
/// cwd that IS home is out of scope — no narrower safe fallback exists.
pub fn workspace_key(cwd: &Path) -> PathBuf {
    let key = git_derived_workspace_key(cwd);
    if is_unsafe_trust_root(&key) {
        return canonicalize_or_owned(cwd);
    }
    key
}

/// Git-topology-derived workspace key (pre-safety-guard); see [`workspace_key`],
/// which rejects an over-broad derived root in favor of the cwd.
fn git_derived_workspace_key(cwd: &Path) -> PathBuf {
    // A echo-agent-managed worktree (any creation mode, incl. standalone clones git
    // can't link) collapses onto its recorded source repo so trust is shared.
    if let Some(source_repo) = crate::worktree::source_repo_for_cwd(&cwd.to_string_lossy()) {
        // Key on the source repo's git ROOT so every worktree of one repo shares
        // ONE key regardless of the subdir echoagent -w was launched from (parity with
        // the git-topology branch below). Fall back to the recorded path when the
        // source repo is gone (deleted-source standalone worktrees still work).
        let root = git2::Repository::discover(&source_repo)
            .ok()
            .and_then(|r| r.workdir().map(canonicalize_or_owned));
        return root.unwrap_or_else(|| canonicalize_or_owned(&source_repo));
    }
    if let Ok(repo) = git2::Repository::discover(cwd) {
        // Share one trust key across a repo's worktrees instead of re-prompting per worktree.
        if repo.is_worktree()
            && let Ok(main) = git2::Repository::open(repo.commondir())
            && let Some(main_workdir) = main.workdir()
            && canonicalize_or_owned(&main_workdir.join(".git"))
                == canonicalize_or_owned(repo.commondir())
        {
            return canonicalize_or_owned(main_workdir);
        }
        if let Some(workdir) = repo.workdir() {
            return canonicalize_or_owned(workdir);
        }
    }
    canonicalize_or_owned(cwd)
}

/// Whether `path` resolves to the user's home directory.
pub fn is_home_dir(path: &Path) -> bool {
    let Some(home) = echo_agent_config::discovery_home_dir() else {
        return false;
    };
    canonicalize_or_owned(path) == canonicalize_or_owned(&home)
}

/// Whether `key` is too broad to ever be a safe trust root — refused on write
/// and ignored on read (fail closed).
///
/// Each case would trust huge swaths of the filesystem via the subdirectory
/// cascade in [`TrustStore::is_trusted`]:
/// - **empty / relative** — the empty path is a prefix of every path, so it
///   would trust everything (`is_absolute()` is false for these);
/// - **filesystem root** (`/`) — `parent()` is `None`, and the root is a prefix
///   of every absolute path, so it would trust the entire filesystem;
/// - **home directory** — would trust every repository checked out under
///   `$HOME`.
///
/// Also consumed by [`crate::folder_trust`] as the "key can never be recorded"
/// signal: such a key can't be durably gated, so it resolves Trusted instead of
/// prompting on a decision that could never persist. Public because the shell's
/// revoke path refuses the same roots symmetrically — an in-process cache deny
/// for a key the store can never grant would be unliftable.
pub fn is_unsafe_trust_root(key: &Path) -> bool {
    !key.is_absolute() || key.parent().is_none() || is_home_dir(key)
}

fn canonicalize_or_owned(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn now_unix() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// RAII exclusive advisory lock on a sidecar lock file, released on drop.
///
/// Serializes concurrent `TrustStore` writers (multiple processes / instances
/// sharing `~/.echo-agent/`) across the whole read-modify-write so updates merge
/// instead of clobbering each other. The lock is advisory; only writers that
/// take it (i.e. this code) coordinate, which is sufficient since this store is
/// the sole writer of its file.
struct ExclusiveLock {
    file: std::fs::File,
}

impl ExclusiveLock {
    fn acquire(lock_path: &Path) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        // Best-effort unlock; the OS also releases the flock when `file` closes.
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// One-time migration of legacy project-hook trust grants into the unified
/// folder-trust store. Idempotent and guarded to run at most once per process.
///
/// The legacy `~/.echo-agent/trusted-hook-projects` file listed one canonical project
/// path per line; each becomes a folder-trust grant so the unified gate honors
/// prior decisions. The legacy file is then renamed to `*.migrated` so it is
/// read only once. A no-op when the legacy file is absent/already migrated or no
/// user echoagent home resolves.
pub fn migrate_legacy_hook_trust() {
    // Local/dev builds do NO trust-store I/O: skip the load + legacy-file rename.
    if crate::folder_trust::folder_trust_inert() {
        return;
    }
    static MIGRATED: Once = Once::new();
    MIGRATED.call_once(|| {
        let Some(legacy_file) = echo_agent_hooks::trust::legacy_trust_file_path() else {
            return;
        };
        let mut store = TrustStore::load();
        let migrated = migrate_legacy_hook_trust_in(&legacy_file, &mut store);
        if migrated > 0 {
            tracing::info!(
                migrated,
                "migrated legacy hook-trust grants into folder-trust"
            );
        }
    });
}

/// Seam for [`migrate_legacy_hook_trust`] with explicit paths, so the migration
/// is testable without the process-global echo-agent-home cache. Returns the number
/// of grants seeded into `store`.
fn migrate_legacy_hook_trust_in(legacy_file: &Path, store: &mut TrustStore) -> usize {
    // A read error must NOT be mistaken for "no grants": bail without renaming so
    // a transient/permission failure can't permanently consume the legacy file.
    let projects = match echo_agent_hooks::trust::list_trusted_projects_with_file(legacy_file) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                path = %legacy_file.display(),
                error = %e,
                "failed to read legacy hook-trust file; leaving it in place for a future run"
            );
            return 0;
        }
    };
    let mut migrated = 0;
    let mut had_seed_error = false;
    for project in &projects {
        // Never override an existing decision: a folder the user has since trusted
        // or untrusted keeps that decision, so a re-run after a rename failure
        // can't silently re-trust a folder the user untrusted in between.
        if store.has_decision(project) {
            continue;
        }
        if let Err(e) = store.set_trusted(project) {
            tracing::warn!(
                path = %project.display(),
                error = %e,
                "failed to migrate a legacy hook-trust grant"
            );
            // A seeding WRITE failure (e.g. a full disk) dropped this grant: leave
            // the legacy file in place below so a future run retries it.
            had_seed_error = true;
            continue;
        }
        // set_trusted silently refuses over-broad roots (records nothing), so
        // count only folders actually seeded.
        if store.has_decision(project) {
            migrated += 1;
        }
    }
    // Rename so the legacy file is consumed exactly once, reached only after a
    // SUCCESSFUL read AND with every grant seeded — a seeding write error leaves
    // the file in place for a future run (mirrors the read-error bail). Idempotent:
    // skipped when already migrated/absent.
    if had_seed_error {
        tracing::warn!(
            path = %legacy_file.display(),
            "leaving legacy hook-trust file in place after a seeding error; a future run will retry"
        );
    } else if legacy_file.exists() {
        let migrated_file = legacy_file.with_extension("migrated");
        if let Err(e) = std::fs::rename(legacy_file, &migrated_file) {
            tracing::warn!(
                path = %legacy_file.display(),
                error = %e,
                "failed to rename legacy hook-trust file after migration"
            );
        }
    }
    migrated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_legacy_hook_trust_seeds_store_and_renames_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        // A real project dir so set_trusted's canonicalize succeeds.
        let project = tmp.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        let project_key = canonicalize_or_owned(&project);

        // Legacy file with one canonical project path.
        let legacy = tmp.path().join("trusted-hook-projects");
        std::fs::write(&legacy, format!("{}\n", project_key.display())).unwrap();

        let mut store = TrustStore::load_from(store_path.clone());
        let migrated = migrate_legacy_hook_trust_in(&legacy, &mut store);
        assert_eq!(migrated, 1);
        assert!(store.is_trusted(&project_key), "migrated grant is trusted");

        // Legacy file renamed so it is read only once.
        assert!(!legacy.exists(), "legacy file consumed");
        assert!(
            legacy.with_extension("migrated").exists(),
            "renamed to .migrated"
        );

        // The grant persisted to disk.
        let reloaded = TrustStore::load_from(store_path);
        assert!(reloaded.is_trusted(&project_key));
    }

    #[test]
    fn migrate_legacy_hook_trust_does_not_override_existing_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let project = tmp.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        let project_key = canonicalize_or_owned(&project);

        let legacy = tmp.path().join("trusted-hook-projects");
        std::fs::write(&legacy, format!("{}\n", project_key.display())).unwrap();

        // The user has ALREADY untrusted this folder in the unified store.
        let mut store = TrustStore::load_from(store_path);
        store.set_untrusted(&project_key).unwrap();

        let migrated = migrate_legacy_hook_trust_in(&legacy, &mut store);
        assert_eq!(migrated, 0, "an already-decided folder is not re-seeded");
        assert!(
            !store.is_trusted(&project_key),
            "the user's untrust decision is preserved, not overridden by migration"
        );
        // The legacy file is still consumed (renamed) so it is read only once.
        assert!(!legacy.exists());
        assert!(legacy.with_extension("migrated").exists());
    }

    #[test]
    fn migrate_legacy_hook_trust_is_noop_when_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let legacy = tmp.path().join("trusted-hook-projects"); // never created

        let mut store = TrustStore::load_from(store_path);
        let migrated = migrate_legacy_hook_trust_in(&legacy, &mut store);
        assert_eq!(migrated, 0);
        assert!(store.is_empty(), "nothing recorded");
        assert!(
            !legacy.with_extension("migrated").exists(),
            "no rename without source"
        );
    }

    #[test]
    fn migrate_legacy_hook_trust_leaves_unreadable_file_in_place() {
        // A legacy file that EXISTS but can't be read must not be consumed: a
        // transient read error would otherwise rename it and permanently drop
        // every grant. Use a directory at the legacy path — it `exists()` but
        // `read_to_string` errors (non-NotFound), portably simulating the failure.
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let legacy = tmp.path().join("trusted-hook-projects");
        std::fs::create_dir_all(&legacy).unwrap();

        let mut store = TrustStore::load_from(store_path);
        let migrated = migrate_legacy_hook_trust_in(&legacy, &mut store);
        assert_eq!(migrated, 0, "an unreadable legacy file seeds nothing");
        assert!(store.is_empty(), "nothing recorded on a read failure");
        assert!(legacy.exists(), "unreadable legacy file is left in place");
        assert!(
            !legacy.with_extension("migrated").exists(),
            "unreadable legacy file must not be consumed/renamed"
        );
    }

    #[test]
    fn migrate_legacy_hook_trust_leaves_file_in_place_on_seed_write_error() {
        // A seeding WRITE failure (e.g. a full disk) must not consume the legacy
        // file either: leave it un-renamed so a future run retries the grants.
        // Force set_trusted to error by making the store path a DIRECTORY so its
        // atomic persist rename fails (same trick as
        // `persist_failure_leaves_memory_unchanged`, robust even when run as root).
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        std::fs::create_dir_all(&store_path).unwrap(); // store path is a dir, not a file
        let project = tmp.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        let project_key = canonicalize_or_owned(&project);

        let legacy = tmp.path().join("trusted-hook-projects");
        std::fs::write(&legacy, format!("{}\n", project_key.display())).unwrap();

        let mut store = TrustStore::load_from(store_path);
        let migrated = migrate_legacy_hook_trust_in(&legacy, &mut store);
        assert_eq!(migrated, 0, "a seeding write error seeds nothing");
        assert!(
            legacy.exists(),
            "legacy file is left in place on a seeding write error"
        );
        assert!(
            !legacy.with_extension("migrated").exists(),
            "a seeding write error must not consume/rename the legacy file"
        );
    }

    #[test]
    fn empty_store_trusts_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TrustStore::load_from(tmp.path().join(TRUST_FILE_NAME));
        assert!(store.is_empty());
        assert!(!store.is_trusted(tmp.path()));
    }

    #[test]
    fn default_path_in_maps_home_and_preserves_no_home() {
        // With a resolvable home the store sits at <home>/trusted_folders.toml.
        let home = PathBuf::from("/home/alice/.echo-agent");
        assert_eq!(
            TrustStore::default_path_in(Some(home.clone())),
            Some(home.join(TRUST_FILE_NAME))
        );

        // With NO resolvable home the path is `None` — never a synthesized
        // fallback. This is the regression guard that keeps the store off the
        // cwd-relative `./.echo-agent` that echo_agent_home() would invent, which is exactly
        // how a cloned repo's own `<repo>/.echo-agent/trusted_folders.toml` could
        // masquerade as the user-global store and self-trust the checkout.
        assert_eq!(TrustStore::default_path_in(None), None);
    }

    #[test]
    fn default_path_sources_from_user_echo_agent_home() {
        // Thin source-pin: the production accessor reads user_echo_agent_home()
        // (Option, no cwd fallback), not echo_agent_home(). The real regression guard
        // is the seam test above (default_path_in(None) == None).
        assert_eq!(
            TrustStore::default_path(),
            echo_agent_config::user_echo_agent_home().map(|h| h.join(TRUST_FILE_NAME))
        );
    }

    #[test]
    fn no_home_store_trusts_nothing_and_persists_nothing() {
        // Simulate the no-home environment where `default_path()` is `None`:
        // `load()` yields `empty()`, a store with no backing path. It must
        // trust nothing and silently no-op on writes — never touching a
        // cwd-relative `./.echo-agent`.
        let mut store = TrustStore::empty();
        assert!(store.is_empty());

        let key = Path::new("/some/abs/repo");
        assert!(!store.is_trusted(key), "no-home store trusts nothing");

        // set_trusted is a no-op that returns Ok and records nothing.
        store
            .set_trusted(key)
            .expect("no-home set_trusted is a no-op Ok");
        assert!(
            store.is_empty(),
            "no-home set_trusted must record nothing (in memory)"
        );
        assert!(
            !store.is_trusted(key),
            "still trusts nothing after the no-op write"
        );

        // set_untrusted likewise no-ops without panicking or recording.
        store
            .set_untrusted(key)
            .expect("no-home set_untrusted is a no-op Ok");
        assert!(store.is_empty());
    }

    #[test]
    fn set_trusted_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let key = canonicalize_or_owned(&repo);

        let mut store = TrustStore::load_from(store_path.clone());
        assert!(!store.is_trusted(&key));
        store.set_trusted(&key).unwrap();
        assert!(store.is_trusted(&key));

        // Reload from disk and verify persistence.
        let reloaded = TrustStore::load_from(store_path);
        assert!(reloaded.is_trusted(&key));
    }

    #[test]
    fn persist_overwrites_existing_and_round_trips_both() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let repo_a = tmp.path().join("repo-a");
        let repo_b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        let key_a = canonicalize_or_owned(&repo_a);
        let key_b = canonicalize_or_owned(&repo_b);

        let mut store = TrustStore::load_from(store_path.clone());
        store.set_trusted(&key_a).unwrap();
        // The second persist runs over an already-existing destination file.
        store.set_trusted(&key_b).unwrap();

        // Both decisions survive the overwrite, after reloading from disk.
        let reloaded = TrustStore::load_from(store_path.clone());
        assert!(reloaded.is_trusted(&key_a));
        assert!(reloaded.is_trusted(&key_b));

        // The owner-only guarantee still holds after the overwrite, independent
        // of umask (NamedTempFile creates 0600 on Unix regardless of umask).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&store_path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "trust store must stay 0600 after overwrite"
            );
        }
    }

    #[test]
    fn trust_cascades_to_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let child = repo.join("crates").join("inner");
        std::fs::create_dir_all(&child).unwrap();
        let repo_key = canonicalize_or_owned(&repo);
        let child_key = canonicalize_or_owned(&child);

        let mut store = TrustStore::load_from(tmp.path().join(TRUST_FILE_NAME));
        store.set_trusted(&repo_key).unwrap();

        // Child dir is trusted because an ancestor (the repo root) is trusted.
        assert!(store.is_trusted(&child_key));
        // A sibling outside the trusted root is NOT trusted.
        let sibling = canonicalize_or_owned(tmp.path()).join("other-repo");
        assert!(!store.is_trusted(&sibling));
        // A sibling that *string*-prefixes the trusted root must NOT be trusted:
        // the cascade is component-wise (`Path::starts_with`), so `…/repo` must
        // not trust `…/repo-sibling` or `…/repository`.
        let prefix_sibling = canonicalize_or_owned(tmp.path()).join("repo-sibling");
        assert!(
            !store.is_trusted(&prefix_sibling),
            "string-prefix sibling must NOT be trusted (cascade is component-wise)"
        );
    }

    #[test]
    fn most_specific_decision_wins_over_ancestor_cascade() {
        // An explicit child untrust must override a trusted ancestor (the bug
        // where an untrust was undone by the cascade on the next reload). The
        // longest-prefix match decides, so siblings of the untrusted child stay
        // trusted via the ancestor.
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        let child = parent.join("child");
        let other = parent.join("other");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let parent_key = canonicalize_or_owned(&parent);
        let child_key = canonicalize_or_owned(&child);
        let other_key = canonicalize_or_owned(&other);

        let mut store = TrustStore::load_from(tmp.path().join(TRUST_FILE_NAME));
        store.set_trusted(&parent_key).unwrap();
        store.set_untrusted(&child_key).unwrap();

        assert!(store.is_trusted(&parent_key), "the ancestor stays trusted");
        assert!(
            !store.is_trusted(&child_key),
            "an explicit child untrust overrides the trusted ancestor"
        );
        assert!(
            !store.is_trusted(&child_key.join("nested")),
            "the untrust cascades to the child's own subdirectories"
        );
        assert!(
            store.is_trusted(&other_key),
            "a sibling without its own decision is still trusted via the ancestor"
        );

        // The most-specific-wins decision survives a reload from disk.
        let reloaded = TrustStore::load_from(tmp.path().join(TRUST_FILE_NAME));
        assert!(!reloaded.is_trusted(&child_key));
        assert!(reloaded.is_trusted(&other_key));
    }

    #[test]
    fn most_specific_trust_wins_over_untrusted_ancestor() {
        // The symmetric half of most-specific-wins: an UNTRUSTED ancestor plus a
        // nearer TRUSTED child => the child IS trusted (the nearer decision wins),
        // and that trust cascades to the child's own subdirectories.
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let parent_key = canonicalize_or_owned(&parent);
        let child_key = canonicalize_or_owned(&child);

        let mut store = TrustStore::load_from(tmp.path().join(TRUST_FILE_NAME));
        store.set_untrusted(&parent_key).unwrap();
        store.set_trusted(&child_key).unwrap();

        assert!(
            !store.is_trusted(&parent_key),
            "the ancestor stays untrusted"
        );
        assert!(
            store.is_trusted(&child_key),
            "a nearer explicit trust overrides the untrusted ancestor"
        );
        assert!(
            store.is_trusted(&child_key.join("nested")),
            "the child's trust cascades to its own subdirectories"
        );

        // The decision survives a reload from disk.
        let reloaded = TrustStore::load_from(tmp.path().join(TRUST_FILE_NAME));
        assert!(!reloaded.is_trusted(&parent_key));
        assert!(reloaded.is_trusted(&child_key));
    }

    #[cfg(unix)]
    #[test]
    fn persisted_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let mut store = TrustStore::load_from(store_path.clone());
        store.set_trusted(&canonicalize_or_owned(&repo)).unwrap();

        let mode = std::fs::metadata(&store_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "trust store must be 0600");
    }

    #[test]
    fn home_dir_is_not_persisted() {
        // Serialize with the in-file $HOME mutator: its temp $HOME window could
        // otherwise flip is_home_dir mid-test. This test mutates no env itself.
        let _lock = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let Some(home) = echo_agent_config::discovery_home_dir() else {
            return; // no home dir in this environment; nothing to assert
        };

        let mut store = TrustStore::load_from(store_path.clone());
        store.set_trusted(&home).unwrap();

        // Nothing was persisted, and the store still holds no folders.
        assert!(store.is_empty(), "home dir must not be recorded");
        assert!(
            !store_path.exists(),
            "no trust file should be written for the home dir"
        );
    }

    #[test]
    fn workspace_key_falls_back_to_cwd_outside_repo() {
        // A freshly created temp dir is not inside a git repo in CI sandboxes;
        // the key should be the canonicalized dir itself.
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("plain");
        std::fs::create_dir_all(&sub).unwrap();
        let key = workspace_key(&sub);
        assert!(key.is_absolute());
        // Only pin the fallback when the temp dir is genuinely outside any git
        // repo (a dev/CI checkout may place $TMPDIR inside the source repository).
        if git2::Repository::discover(&sub).is_err() {
            assert_eq!(key, canonicalize_or_owned(&sub));
        }
    }

    #[test]
    fn workspace_key_ignores_home_git_repo_for_subdir() {
        // Home-is-a-git-repo (dotfiles in $HOME): a subdir launched from under
        // home must key trust on the SUBDIR, not on $HOME — even though the git
        // up-walk discovers home as the repo root. Serialize + guard $HOME
        // (dirs::home_dir reads it) via the crate-shared env lock.
        let _lock = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home_guard = crate::TestEnvGuard::set("HOME", home.path());
        git2::Repository::init(home.path()).unwrap();
        let civ = home.path().join("Documents").join("civ");
        std::fs::create_dir_all(&civ).unwrap();

        let key = workspace_key(&civ);
        assert_eq!(
            key,
            canonicalize_or_owned(&civ),
            "a subdir under a home git repo must key on the subdir, not $HOME"
        );
        assert!(
            !is_home_dir(&key),
            "the workspace key must never resolve to the home dir"
        );
    }

    #[test]
    fn empty_key_is_not_trusted() {
        // Fail closed: a degenerate `[folders.""] trusted = true` must not trust
        // anything. The empty path is a prefix of every path, so honoring it
        // would trust the whole filesystem (fail open).
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        std::fs::write(&store_path, "[folders.\"\"]\ntrusted = true\n").unwrap();

        let store = TrustStore::load_from(store_path);
        // The record loads, so this exercises the read-side guard (not a parse drop).
        assert!(!store.is_empty(), "empty-key record should still load");
        assert!(
            !store.is_trusted(Path::new("/some/arbitrary/path")),
            "an empty key must not trust the filesystem"
        );
    }

    #[test]
    fn malformed_store_fails_soft_to_empty() {
        // Corrupt TOML must fail closed: empty store, trust nothing, no panic.
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        std::fs::write(&store_path, "this is not = valid toml [[[").unwrap();

        let store = TrustStore::load_from(store_path);
        assert!(store.is_empty(), "malformed store must load as empty");
        assert!(!store.is_trusted(Path::new("/any/path")));
    }

    #[test]
    fn root_key_is_not_trusted() {
        // Fail closed: a `[folders."/"]` record must not trust every absolute
        // path. The root is a prefix of all of them via the cascade, so it is
        // ignored on read even if it reaches the file by hand-edit / migration.
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        std::fs::write(&store_path, "[folders.\"/\"]\ntrusted = true\n").unwrap();

        let store = TrustStore::load_from(store_path);
        assert!(!store.is_empty(), "root-key record should still load");
        assert!(
            !store.is_trusted(Path::new("/any/abs/path")),
            "filesystem root must never be honored as a trust key"
        );
    }

    #[test]
    fn tied_conflicting_aliases_fail_closed() {
        // Two equal-depth, non-canonical aliases of the SAME folder with CONFLICTING
        // decisions: `Path::components()` normalizes the trailing slash so `/a/b` and
        // `/a/b/` tie on depth, yet they load as distinct map keys. The tie branch
        // ANDs the tied records, so any untrusted tied alias forces a fail-closed
        // `false` REGARDLESS of map order. Asserting BOTH orderings is what pins
        // this: a revert to a last-wins form (return the LAST equal-depth record,
        // i.e. `/a/b/`) returns `true` for ordering (b) below and fails this test,
        // whereas pinning a single ordering would pass under both the AND-loop and
        // the buggy last-wins form.
        let fails_closed = |trusted_ab: bool, trusted_ab_slash: bool| {
            let tmp = tempfile::tempdir().unwrap();
            let store_path = tmp.path().join(TRUST_FILE_NAME);
            std::fs::write(
                &store_path,
                format!(
                    "[folders.'/a/b']\ntrusted = {trusted_ab}\n\
                     [folders.'/a/b/']\ntrusted = {trusted_ab_slash}\n"
                ),
            )
            .unwrap();
            let store = TrustStore::load_from(store_path);
            assert_eq!(store.len(), 2, "both alias records should load distinctly");
            // `/a/b/c` does not exist, so `canonicalize_or_owned` is a no-op; both
            // aliases prefix it and tie on depth.
            !store.is_trusted(Path::new("/a/b/c"))
        };

        // (a) untrusted alias sorts LAST (`/a/b/`): caught by a revert to the
        //     original `any(trusted)` form, but NOT by a last-wins revert.
        assert!(
            fails_closed(true, false),
            "tie with `/a/b` trusted + `/a/b/` untrusted must fail closed"
        );
        // (b) untrusted alias sorts FIRST (`/a/b`): a last-wins revert returns the
        //     last record (`/a/b/` = trusted) => true, so THIS ordering is what
        //     catches a last-wins regression; the AND-loop still yields false.
        assert!(
            fails_closed(false, true),
            "tie with `/a/b` untrusted + `/a/b/` trusted must STILL fail closed"
        );
    }

    #[test]
    fn home_key_on_disk_is_not_honored() {
        // Serialize with the in-file $HOME mutator: its temp $HOME window could
        // otherwise flip is_home_dir mid-test. This test mutates no env itself.
        let _lock = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // A hand-edited / migrated `[folders."<home>"]` record must not trust
        // repos under $HOME — the read side ignores it, matching set_trusted.
        let Some(home) = echo_agent_config::discovery_home_dir() else {
            return; // no home dir in this environment; nothing to assert
        };
        let canonical_home = canonicalize_or_owned(&home);
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        // TOML literal-string key avoids escaping issues on any platform.
        let body = format!(
            "[folders.'{}']\ntrusted = true\n",
            canonical_home.to_string_lossy()
        );
        std::fs::write(&store_path, body).unwrap();

        let store = TrustStore::load_from(store_path);
        let sub = canonical_home.join("some").join("sub");
        assert!(
            !store.is_trusted(&sub),
            "a home-dir key on disk must not be honored"
        );
    }

    #[cfg(unix)]
    #[test]
    fn set_trusted_canonicalizes_key() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let real = tmp.path().join("real-repo");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link-repo");
        symlink(&real, &link).unwrap();

        // Trust via the symlink alias.
        let mut store = TrustStore::load_from(store_path);
        store.set_trusted(&link).unwrap();

        // The stored key was canonicalized, so a canonical lookup matches.
        let canonical_real = canonicalize_or_owned(&real);
        assert!(
            store.is_trusted(&canonical_real),
            "set_trusted must store the canonical path so canonical lookups match"
        );
    }

    #[test]
    fn set_untrusted_records_explicit_deny() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let key = canonicalize_or_owned(&repo);

        let mut store = TrustStore::load_from(store_path.clone());
        store.set_untrusted(&key).unwrap();
        assert!(!store.is_trusted(&key), "an explicit deny is not trusted");
        assert!(!store.is_empty(), "the deny decision is recorded");

        // Reload from disk: the deny record persisted.
        let reloaded = TrustStore::load_from(store_path);
        assert!(!reloaded.is_trusted(&key));
        assert!(!reloaded.is_empty(), "deny record survives reload");
    }

    #[test]
    fn trust_decision_flips() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let key = canonicalize_or_owned(&repo);

        let mut store = TrustStore::load_from(store_path.clone());
        store.set_trusted(&key).unwrap();
        assert!(store.is_trusted(&key));
        store.set_untrusted(&key).unwrap();
        assert!(!store.is_trusted(&key), "untrust flips the stored bool");
        store.set_trusted(&key).unwrap();
        assert!(store.is_trusted(&key), "re-trust flips it back");
        // The insert overwrites — one record per folder, no duplicates.
        assert_eq!(store.len(), 1);

        let reloaded = TrustStore::load_from(store_path);
        assert!(reloaded.is_trusted(&key));
        assert_eq!(reloaded.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn is_trusted_canonicalizes_query() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let real = tmp.path().join("real-repo");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link-repo");
        symlink(&real, &link).unwrap();

        let mut store = TrustStore::load_from(store_path);
        store.set_trusted(&canonicalize_or_owned(&real)).unwrap();

        // A query via the symlink alias resolves to the trusted real dir.
        assert!(
            store.is_trusted(&link),
            "is_trusted must canonicalize the query so a symlink alias matches"
        );
    }

    #[test]
    fn concurrent_writers_do_not_clobber() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        let repo_a = tmp.path().join("repo-a");
        let repo_b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        let key_a = canonicalize_or_owned(&repo_a);
        let key_b = canonicalize_or_owned(&repo_b);

        // Two instances loaded while the file is empty: both start with an empty
        // in-memory doc, mimicking two processes that raced the initial load.
        let mut s1 = TrustStore::load_from(store_path.clone());
        let mut s2 = TrustStore::load_from(store_path.clone());
        s1.set_trusted(&key_a).unwrap();
        s2.set_trusted(&key_b).unwrap();

        // The locked re-read-merge means s2's write did not clobber s1's.
        let reloaded = TrustStore::load_from(store_path);
        assert!(
            reloaded.is_trusted(&key_a),
            "A must survive a concurrent write"
        );
        assert!(
            reloaded.is_trusted(&key_b),
            "B must survive a concurrent write"
        );
    }

    #[cfg(unix)]
    #[test]
    fn persist_failure_leaves_memory_unchanged() {
        // Make the destination path itself a DIRECTORY so the final atomic
        // rename in persist fails (renaming a file over a directory). This is
        // robust even when tests run as root (a chmod 0o500 dir would be
        // bypassed by root), and it exercises the invariant: on a write error
        // the in-memory doc is left unchanged (memory-before-persist fix).
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join(TRUST_FILE_NAME);
        std::fs::create_dir_all(&store_path).unwrap(); // store path is a dir, not a file
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let key = canonicalize_or_owned(&repo);

        let mut store = TrustStore::load_from(store_path);
        let result = store.set_trusted(&key);
        assert!(
            result.is_err(),
            "persist over a directory destination must fail"
        );
        assert!(
            !store.is_trusted(&key),
            "memory must be unchanged on persist failure"
        );
    }

    #[test]
    fn workspace_key_collapses_linked_worktrees_onto_main_checkout() {
        // Every linked `echoagent -w` worktree of a repo must share ONE trust key:
        // its main checkout's root. Build a real repo + two linked worktrees and
        // assert each collapses onto the main checkout (so it is trusted once,
        // not re-prompted per worktree).
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let repo = git2::Repository::init(&main).unwrap();

        // Worktree creation requires a valid HEAD, so make an initial commit.
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree = {
            let mut idx = repo.index().unwrap();
            let oid = idx.write_tree().unwrap();
            repo.find_tree(oid).unwrap()
        };
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Linked worktrees OUTSIDE the main dir (git2 creates the paths).
        let wt1 = dir.path().join("wt1");
        let wt2 = dir.path().join("wt2");
        repo.worktree("wt1", &wt1, None).unwrap();
        repo.worktree("wt2", &wt2, None).unwrap();

        let main_key = workspace_key(&main);
        // Parity: the main checkout keys off its own workdir.
        assert_eq!(main_key, canonicalize_or_owned(&main));
        assert_eq!(
            workspace_key(&wt1),
            main_key,
            "worktree must collapse onto main checkout"
        );
        assert_eq!(
            workspace_key(&wt2),
            main_key,
            "second worktree must share the same key"
        );
    }

    #[test]
    fn workspace_key_bare_repo_worktree_does_not_widen_to_parent() {
        // A bare repo's `commondir()` is the bare dir itself, so a naive
        // `commondir().parent()` would key off the dir CONTAINING the repo and
        // trust every sibling via the subdirectory cascade. The key must instead
        // fall back to the worktree's OWN dir (narrow, never widened).
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("repo.git");
        let repo = git2::Repository::init_bare(&bare).unwrap();

        // Worktree creation needs a valid HEAD; build an empty commit (bare repo
        // has no index, so use a treebuilder for the empty tree).
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree_oid = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let wt = dir.path().join("wt");
        repo.worktree("wt", &wt, None).unwrap();

        let key = workspace_key(&wt);
        assert_ne!(
            key,
            canonicalize_or_owned(dir.path()),
            "bare-repo worktree key must not widen to the parent dir"
        );
        assert_eq!(
            key,
            canonicalize_or_owned(&wt),
            "bare-repo worktree falls back to its own dir (narrow, safe)"
        );
    }

    #[test]
    fn workspace_key_separate_gitdir_worktree_does_not_widen() {
        // `git init --separate-git-dir` leaves `core.worktree` unset, so the
        // common gitdir's INFERRED workdir is the PARENT of the relocated gitdir,
        // not the checkout. The layout guard (`<workdir>/.git` must equal the
        // common gitdir) rejects that, so the key falls back to the worktree's
        // own dir — never widening to the gitdir's parent.
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        let gitdir = dir.path().join("gitstore");
        std::fs::create_dir_all(&checkout).unwrap();
        let run = |args: &[&str], cwd: &std::path::Path| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        // If git isn't usable for this layout, skip rather than false-fail.
        if !run(
            &[
                "init",
                "--separate-git-dir",
                gitdir.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
            dir.path(),
        ) || !run(&["commit", "--allow-empty", "-m", "init"], &checkout)
        {
            return;
        }
        let wt = dir.path().join("wt");
        if !run(&["worktree", "add", wt.to_str().unwrap()], &checkout) {
            return;
        }

        // Only assert once the worktree is a real linked worktree whose common
        // gitdir is the relocated separate gitdir (the layout this test targets).
        let Ok(repo) = git2::Repository::discover(&wt) else {
            return;
        };
        if !repo.is_worktree() {
            return;
        }

        let key = workspace_key(&wt);
        // The invariant: the key is NOT a broad ancestor of the checkout.
        assert_ne!(
            key,
            canonicalize_or_owned(dir.path()),
            "separate-gitdir worktree key must not widen to the gitdir's parent"
        );
        // It falls back to the worktree's own dir (narrow, safe).
        assert_eq!(
            key,
            canonicalize_or_owned(&wt),
            "separate-gitdir worktree falls back to its own dir (narrow, safe)"
        );
    }

    // ── workspace_key registry collapse (echo-agent-managed worktrees) ─────────

    // Crate-shared env lock + env guards bundled as ONE value so the env restores
    // before the lock releases by struct field order (see lib.rs), regardless of
    // how the caller binds the fixture's return.
    use crate::LockedTestEnv;

    /// Point `ECHO_AGENT_HOME` at an isolated tempdir and register one echo-agent-managed
    /// worktree at `<home>/worktrees/repo/<name>` recording `source_repo` and
    /// `creation_mode`. The worktree dir is a PLAIN directory — NOT a git linked
    /// worktree — so only the registry can collapse it. Returns `(env, worktree
    /// dir)`; the [`LockedTestEnv`] holds the lock and restores `ECHO_AGENT_HOME` on
    /// drop (before releasing the lock), so the caller may bind it any way.
    fn register_echo_agent_worktree(
        temp: &tempfile::TempDir,
        name: &str,
        source_repo: &Path,
        creation_mode: &str,
    ) -> (LockedTestEnv, PathBuf) {
        use echo_agent_fast_worktree::{WorktreeDb, WorktreeKind, WorktreeRecord, WorktreeStatus};

        // Canonicalize so macOS /var -> /private/var agrees between the stored
        // record path and the canonicalized lookup query.
        let root = dunce::canonicalize(temp.path()).unwrap();
        let home = root.join("echo-agent-home");
        let wt = home.join("worktrees").join("repo").join(name);
        std::fs::create_dir_all(&wt).unwrap();

        // Acquire the lock, then set the env under it (LockedTestEnv restores the
        // env before releasing the lock on drop).
        let env = LockedTestEnv::lock().set("ECHO_AGENT_HOME", &home);

        let db = WorktreeDb::open(&home).unwrap();
        let record = WorktreeRecord {
            id: name.to_string(),
            path: wt.clone(),
            source_repo: source_repo.to_path_buf(),
            repo_name: "repo".to_string(),
            kind: WorktreeKind::Session,
            creation_mode: creation_mode.to_string(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: 100,
            last_accessed_at: None,
            status: WorktreeStatus::Alive,
            metadata: None,
        };
        db.register(&record).unwrap();
        (env, wt)
    }

    #[test]
    fn workspace_key_collapses_standalone_echo_agent_worktree_onto_source_repo() {
        // A standalone worktree is a full clone with its OWN `.git`, so git
        // topology can't link it to its source; the registry (worktrees.db) must
        // collapse it onto the recorded source repo so trust is shared. The
        // worktree dir is a plain dir (no git), proving the REGISTRY path — not
        // git topology — does the collapse. `source_repo` is a real git repo (as
        // in production), so the git-root normalization is deterministic
        // regardless of where `$TMPDIR` lives.
        let temp = tempfile::TempDir::new().unwrap();
        let root = dunce::canonicalize(temp.path()).unwrap();
        let source_repo = root.join("source-repo");
        std::fs::create_dir_all(&source_repo).unwrap();
        git2::Repository::init(&source_repo).unwrap();

        let (_env, wt) = register_echo_agent_worktree(&temp, "wt", &source_repo, "standalone");

        let expected = canonicalize_or_owned(&source_repo);
        assert_eq!(
            workspace_key(&wt),
            expected,
            "a standalone echoagent worktree must collapse onto its recorded source repo"
        );
        // A cwd nested below the worktree root collapses onto the same key (the
        // registry walk ascends to the registered worktree).
        let nested = wt.join("crates").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            workspace_key(&nested),
            expected,
            "a nested cwd in the worktree collapses onto the same source repo"
        );
    }

    #[test]
    fn workspace_key_collapses_worktree_onto_source_repo_git_root() {
        // The registry records `source_repo` as the launch cwd, which may be a
        // SUBDIR of the repo. workspace_key must key on the repo's git ROOT so a
        // worktree launched from a subdir shares ONE key with the source and
        // linked worktrees (which key on the root), not on `<repo>/sub`.
        let temp = tempfile::TempDir::new().unwrap();
        let root = dunce::canonicalize(temp.path()).unwrap();
        let repo = root.join("realrepo");
        std::fs::create_dir_all(&repo).unwrap();
        git2::Repository::init(&repo).unwrap();
        let subdir = repo.join("crates").join("sub");
        std::fs::create_dir_all(&subdir).unwrap();

        let (_env, wt) = register_echo_agent_worktree(&temp, "wt", &subdir, "standalone");

        assert_eq!(
            workspace_key(&wt),
            canonicalize_or_owned(&repo),
            "source_repo recorded as a subdir must collapse onto the repo git root"
        );
    }

    #[test]
    fn workspace_key_ignores_registry_for_cwd_outside_worktrees_dir() {
        // A populated registry must NOT collapse a cwd OUTSIDE
        // `<echo_agent_home>/worktrees`: `worktree_record_for_cwd` skips the registry
        // there, so the key falls back to git/cwd. Non-vacuous: the registry IS
        // populated with a real git source repo that WOULD be returned for a
        // worktree cwd, and `outside` is its OWN git repo (under echoagent HOME but not
        // under its `worktrees/`) so the fallback is deterministic (no conditional
        // skip) — we assert the key is `outside`'s own root, never the source repo.
        let temp = tempfile::TempDir::new().unwrap();
        let root = dunce::canonicalize(temp.path()).unwrap();
        let source_repo = root.join("source-repo");
        std::fs::create_dir_all(&source_repo).unwrap();
        git2::Repository::init(&source_repo).unwrap();

        let (_env, _wt) = register_echo_agent_worktree(&temp, "wt", &source_repo, "standalone");

        // Under echoagent HOME but NOT under `<home>/worktrees`, and its own git repo.
        let outside = root
            .join("echo-agent-home")
            .join("not-worktrees")
            .join("proj");
        std::fs::create_dir_all(&outside).unwrap();
        git2::Repository::init(&outside).unwrap();

        let key = workspace_key(&outside);
        assert_eq!(
            key,
            canonicalize_or_owned(&outside),
            "a cwd outside <echo_agent_home>/worktrees keys on its own repo root"
        );
        assert_ne!(
            key,
            canonicalize_or_owned(&source_repo),
            "it must not collapse onto the populated registry's source repo"
        );
    }
}
