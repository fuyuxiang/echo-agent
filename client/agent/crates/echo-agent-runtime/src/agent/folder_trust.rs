//! Folder-trust gate ("do you trust this folder?").
//!
//! Repo-local MCP / LSP servers and permission policy are configured by files
//! an attacker can ship inside a cloned repository (`.mcp.json`, project
//! `.echo-agent/config.toml` including `[permission]` / `[mcp_servers]` /
//! `[plugins].paths`, `~/.claude.json` `projects.<cwd>`, project `.echo-agent/lsp.json`).
//! Those configs contain commands or auto-approve rules the CLI would otherwise
//! honor automatically — a 1-click RCE / policy bypass. This module resolves a
//! VS-Code-style trust decision ONCE per workspace, BEFORE any repo-local
//! server is spawned, and exposes a cheap [`project_scope_allowed`] check that
//! the MCP/LSP/permission loaders consult.
//!
//! Resolution lives here (not in `acp_session`) so the session core stays free
//! of feature logic; the loaders only call [`project_scope_allowed`].
//!
//! The DECISION side — the workspace scan, the pure [`decide`] precedence, the
//! interactive prompt, and the durable [`echo_agent_workspace::trust::TrustStore`]
//! reads/writes — lives in `echo-agent-workspace` (client-side); this module keeps
//! the CONSUME/gating side (the `DECISIONS` cache, [`resolve_and_record`], and
//! the loader filters). The ordered trust precedence is documented canonically
//! on [`echo_agent_workspace::folder_trust::decide`]; the consume-side nuance is
//! that two allows are PROVISIONAL (NOT cached): the "no repo configs" allow — so
//! configs appearing after the first resolve (git pull / agent write) are
//! re-checked on the next resolve rather than riding a stale grant — and the
//! unrecordable-key allow (cwd is $HOME / fs-root), which can never be persisted
//! anyway (see [`resolve_and_record_inner`] / [`compute`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use agent_client_protocol as acp;
use echo_agent_workspace::trust::{TrustStore, is_unsafe_trust_root, workspace_key};
use parking_lot::Mutex;

// Decision-side (scan/decide/prompt/store) relocated to `echo-agent-workspace`
// (client crate). `grant_folder_trust` is the ONLY moved item referenced from
// OUTSIDE this module (shell call sites + the pager's
// `echo_agent_runtime::agent::folder_trust::grant_folder_trust`), so only it is
// re-published; the rest are private imports used within this module. A glob
// re-export is deliberately avoided: it would silently re-publish the
// cache-SKIPPING `revoke_folder_trust_store` next to the real
// `revoke_folder_trust` wrapper, inviting a stale-untrust security bug.
pub use echo_agent_workspace::folder_trust::grant_folder_trust;
use echo_agent_workspace::folder_trust::{
    DecideInputs, TrustOutcome, claude_project_mcp_names, decide, decide_inputs,
    decide_inputs_with_interactive, feature_enabled, folder_trust_inert, persist_trust,
    prompt_for_trust,
};

use crate::session::managed_mcp::mcp_server_name;
use crate::util::config::{MCP_SCOPE_PROJECT, RemoteSettings};

// NOTE: this folder-trust store (`~/.echo-agent/trusted_folders.toml`) is SEPARATE
// from the pre-existing per-plugin trust store
// (`echo_agent_core::plugins::TrustStore` at `~/.echo-agent/trusted-plugins`, plus the
// hooks' own project-trust gating). Trusting a folder here does NOT imply plugin
// trust and vice versa; the two are independent and non-contradicting.
// Unifying them is a tracked follow-up (out of scope for this PR).

/// Per-workspace resolved decision: `true` = repo-local (project-scoped)
/// servers are allowed to spawn. Keyed by canonical workspace key.
static DECISIONS: LazyLock<Mutex<HashMap<PathBuf, bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Revoke trust for `cwd`'s workspace: downgrade the in-process decision cache
/// so a mid-session untrust takes effect immediately, while the store half —
/// persisting an explicit `set_untrusted` ONLY when the folder was actually
/// trusted — is delegated to
/// [`echo_agent_workspace::folder_trust::revoke_folder_trust_store`].
///
/// Without the cache downgrade a cached grant would short-circuit
/// [`resolve_and_record`] (which only reconciles untrusted→trusted), so hooks
/// would keep loading until restart. Unrecordable roots ($HOME / fs-root) are
/// refused instead: [`decide`] always trusts them and the store refuses both
/// their grants and denies, so a cache deny would be the one verdict nothing
/// (grant, store, prompt) could ever lift. Returns whether the folder had been
/// trusted. Symmetric with [`grant_folder_trust`].
pub(crate) fn revoke_folder_trust(cwd: &Path) -> bool {
    // Local/dev builds are fully inert: nothing was trusted-via-gate to revoke,
    // and recording `false` here would make `project_scope_allowed` wrongly gate.
    if folder_trust_inert() {
        return false;
    }
    let key = workspace_key(cwd);
    // Mirror the store's over-broad-root refusal: an unrecordable key resolves
    // Trusted by rule and can never be store-granted, so a cache deny here would
    // be permanent for the process with no in-product recovery.
    if is_unsafe_trust_root(&key) {
        tracing::warn!(
            key = %key.display(),
            "refusing folder-trust revoke for an over-broad root (never gated)"
        );
        return false;
    }
    let was_trusted = echo_agent_workspace::folder_trust::revoke_folder_trust_store(cwd);
    // Always downgrade the in-process cache so a mid-session untrust takes effect
    // immediately for this process, even for a cached grant with no backing store
    // record (e.g. a kill-switch / feature-off resolve). A later legitimate grant
    // reconciles it: the `Some(false)` arm of `resolve_and_record_inner` re-checks
    // the store.
    record(&key, false);
    was_trusted
}

/// Whether repo-local (project-scoped) MCP/LSP servers may spawn for `cwd`.
///
/// Authoritative and fail-closed, mirroring [`resolve_and_record_inner`]'s arms:
/// a cached **grant** short-circuits (allow); a cached **untrusted** verdict is
/// RE-READ against the store so a `grant_folder_trust` issued AFTER the untrusted
/// resolve is honored (it records the upgrade and allows); an **unrecorded** key
/// re-resolves via [`resolve_and_record`] — deny ONLY the dangerous case (feature
/// on AND repo-local code-exec configs present AND untrusted); allow no-configs /
/// unrecordable-key ($HOME / fs-root) / store-trusted / feature-off / inert. So
/// this never over-denies the common no-configs case, whose Trusted verdict is
/// provisional and therefore never cached.
///
/// The cache is consulted BEFORE delegating so a recorded `Some(false)` is
/// reconciled even on an inert build, where [`resolve_and_record`] would short-
/// circuit to allow before reaching the cache. `remote = None`: durable
/// feature-off / kill-switch verdicts are already cached by the launch/session
/// resolve that ran with the real RemoteSettings.
///
/// `DECISIONS` uses `parking_lot::Mutex` (no poisoning), so this gate cannot
/// fail OPEN on a poisoned lock.
pub(crate) fn project_scope_allowed(cwd: &Path) -> bool {
    let key = workspace_key(cwd);
    // Copy out of the lock so the Some(false) reconcile can re-acquire it
    // (parking_lot mutexes are not re-entrant).
    let cached = DECISIONS.lock().get(&key).copied();
    match cached {
        Some(true) => true,
        // Re-read the store so a grant issued after the untrusted resolve is
        // honored without a restart (mirrors `resolve_and_record_inner`).
        Some(false) => {
            if TrustStore::load().is_trusted(&key) {
                record(&key, true);
                true
            } else {
                false
            }
        }
        // Unrecorded: re-resolve fail-closed (no-configs / trusted / feature-off /
        // inert allow; untrusted + configs deny).
        None => resolve_and_record(cwd, None, false),
    }
}

/// Whether an interactive GUI trust PROMPT is warranted for `cwd`: the feature
/// is on, the workspace is NOT store-trusted, and repo-local code-exec configs
/// are present (something to gate). Interactivity is forced `true` because the
/// caller already confirmed the client can prompt (it advertised
/// `echo.agent/folderTrust.interactive`); the TTY-based [`decide_inputs`] default is
/// false under the ACP stdio transport. Mirrors the [`decide`] precedence so it
/// cannot drift from the gate: feature-off (kill-switch / opt-out) / store-trusted
/// / no-configs all collapse to a non-`Prompt` verdict and return false.
pub(crate) fn prompt_warranted(cwd: &Path, remote: Option<&RemoteSettings>) -> bool {
    let key = workspace_key(cwd);
    matches!(
        decide(
            feature_enabled(remote),
            &decide_inputs_with_interactive(cwd, &key, true),
        ),
        TrustOutcome::Prompt
    )
}

/// Display-only summary of which repo-local code-exec config kinds are present for
/// `cwd` — the reasons the folder is gated — for the interactive trust prompt's
/// UI. Single-sourced from the SAME scan as the canonical gate
/// ([`echo_agent_workspace::folder_trust::repo_config_kinds`] /
/// [`repo_configs_present`]) so the prompt's reason list cannot drift from what
/// actually gated the folder (same markers, same cwd→git-root walk).
///
/// ALL detected kinds are reported, including `lsp`: it is a genuine reason the
/// folder is gated (so an `.echo-agent/lsp.json`-only repo still has a non-empty reason
/// list). Only the post-grant *hot-reload* skips LSP — project LSP applies on the
/// next session open (the backend is spawn-baked into the tool bridge). See the
/// `mvp_agent::folder_trust_prompt` module docs.
pub(crate) fn detected_config_kinds(cwd: &Path) -> Vec<String> {
    echo_agent_workspace::folder_trust::repo_config_kinds(cwd)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Whether an agent's inline `hooks:` block may be appended to the live hook
/// registry. A PROJECT/cwd-discovered agent's inline hooks are repo-controlled
/// code-exec (and a project agent can SHADOW a built-in subagent, e.g. `explore`),
/// so they require folder trust; user/bundled/built-in agents (not cwd-sourced)
/// always keep theirs. `trusted` is evaluated LAZILY so non-project agents skip
/// the (filesystem-walking) trust verdict entirely. SINGLE definition shared by
/// the primary-session and subagent append sites (and the test) so they cannot
/// drift. The primary site passes its already-computed `hooks_trusted` verdict;
/// the subagent site passes `project_scope_allowed(parent_cwd)`.
pub(crate) fn agent_inline_hooks_allowed(
    scope: echo_agent_core::config::AgentScope,
    trusted: impl FnOnce() -> bool,
) -> bool {
    scope != echo_agent_core::config::AgentScope::Project || trusted()
}

fn record(workspace_key: &Path, allowed: bool) {
    DECISIONS
        .lock()
        .insert(workspace_key.to_path_buf(), allowed);
}

/// Test-only: force the recorded decision for `cwd`'s workspace key.
///
/// Tests use UNIQUE temp-dir keys and never globally clear `DECISIONS`, so they
/// can run in parallel without clobbering each other's recorded decisions.
///
/// Consumed by the MCP project-scope gate tests here and in `managed_mcp`.
#[cfg(test)]
pub(crate) fn record_for_test(cwd: &Path, allowed: bool) {
    record(&workspace_key(cwd), allowed);
}

/// Resolve the trust decision for `cwd` ONCE and record it for the loaders.
///
/// Returns whether project-scoped servers are allowed. A cached **grant**
/// short-circuits; a cached **untrusted** verdict is re-checked against the
/// store so a later `--trust` grant is honored without a restart (see
/// [`resolve_and_record_inner`]). Persists on an accepted interactive prompt; an
/// explicit `--trust` grant is persisted up front by [`grant_folder_trust`].
///
/// `allow_prompt` must be `true` ONLY where a blocking stdin y/N read is safe —
/// i.e. agent `initialize` for the launch directory, before the TUI takes over
/// the terminal. Every other call site (per-session cwd, leader-served sessions
/// whose cwd differs from the launch dir, `echoagent mcp doctor`) passes `false`, so
/// an unresolved interactive-but-untrusted workspace resolves **fail-closed**
/// (untrusted, no prompt) — only the launch dir is ever prompted for.
pub(crate) fn resolve_and_record(
    cwd: &Path,
    remote: Option<&RemoteSettings>,
    allow_prompt: bool,
) -> bool {
    // Local/dev builds are fully inert: project scope is always allowed, so skip
    // the `trusted_folders.toml` read entirely.
    if folder_trust_inert() {
        return true;
    }
    let key = workspace_key(cwd);
    resolve_and_record_inner(
        &key,
        || TrustStore::load().is_trusted(&key),
        || compute(cwd, &key, remote, allow_prompt),
    )
}

/// Resolve the launch dir's project-scope trust verdict with a SINGLE expensive
/// gather, recorded into `DECISIONS` exactly as [`resolve_and_record`] would, so
/// the one-time deferred init helpers can share it. This is the AUTHORITATIVE
/// description of the launch-dir dedup + TOCTOU contract (the `MvpAgent`
/// field/method docs point here).
///
/// The deferred init helpers (`ensure_plugin_registry` and
/// `ensure_local_workspace_ops`) each gather independently, and the provisional
/// "no repo configs" allow is non-durable (never absorbed by the `DECISIONS`
/// cache), so without memoization the launch dir is scanned multiple times
/// during init. This gathers [`decide_inputs`] (store read + `repo_configs_present`
/// scan) ONCE and derives the verdict through the same [`resolve_and_record_inner`]
/// cache contract, so durable verdicts are recorded into `DECISIONS` and the
/// provisional allows (no-configs, unrecordable key) are left uncached.
///
/// TOCTOU: this records ONLY what [`resolve_and_record`] records, so a later
/// per-session `resolve_and_record(session_cwd)` still re-scans the provisional
/// no-configs case (config added post-startup via git pull / agent write is
/// caught). The init-time dedup belongs to the one-shot caller (a `OnceCell` on
/// `MvpAgent`), NOT to any new shared-cache entry.
pub(crate) fn resolve_launch_dir_trust(cwd: &Path, remote: Option<&RemoteSettings>) -> bool {
    // Local/dev builds are fully inert: project scope is always allowed, skipping
    // the store read + repo scan entirely.
    if folder_trust_inert() {
        return true;
    }
    let key = workspace_key(cwd);
    let feature = feature_enabled(remote);
    let inputs = decide_inputs(cwd, &key);
    // Re-read the store for the cached-untrusted reconciliation EXACTLY as
    // resolve_and_record does (so a `--trust` granted after a parallel resolve
    // recorded untrusted is still honored), and reuse the gathered inputs only
    // for the recompute — keeping the DECISIONS cache contract identical to
    // resolve_and_record without repeating the expensive repo_configs scan.
    resolve_and_record_inner(
        &key,
        || TrustStore::load().is_trusted(&key),
        || compute_from_inputs(&inputs, feature, &key, false),
    )
}

/// Cache-reconciling core of [`resolve_and_record`], split out so the
/// invalidation path is testable without the process-global trust store.
///
/// - A cached **grant** (`Some(true)`) is durable and short-circuits — neither
///   `store_trusted` nor `recompute` runs.
/// - A cached **untrusted** verdict (`Some(false)`) is re-checked via
///   `store_trusted`: a `echoagent --trust` grant issued AFTER this workspace was
///   first resolved writes the store, so honor it on the next session without a
///   restart. Without this re-read a long-lived leader would mask the grant.
/// - An **unrecorded** key (`None`) does a full `recompute`, which reports
///   `(allowed, durable)`; the verdict is recorded ONLY when `durable`. The
///   provisional "no repo configs" allow is non-durable, so it stays unrecorded
///   and every resolve re-checks for code-exec config that appeared after the
///   folder was first opened (TOCTOU).
fn resolve_and_record_inner(
    key: &Path,
    store_trusted: impl FnOnce() -> bool,
    recompute: impl FnOnce() -> (bool, bool),
) -> bool {
    // Copy out of the lock so `record` below can re-acquire it (parking_lot
    // mutexes are not re-entrant).
    let cached = DECISIONS.lock().get(key).copied();
    match cached {
        Some(true) => true,
        Some(false) => {
            if store_trusted() {
                record(key, true);
                true
            } else {
                false
            }
        }
        None => {
            // Record only durable verdicts; a provisional no-configs allow is
            // left uncached so the next resolve re-checks `repo_configs_present`.
            let (allowed, durable) = recompute();
            if durable {
                record(key, allowed);
            }
            allowed
        }
    }
}

/// Returns `(allowed, durable)`. `durable` is whether the verdict may be cached.
/// Two allows are NON-durable: (1) the "no repo configs" allow, because
/// repo-local code-exec config can appear after this resolve (git pull / agent
/// write) and caching that provisional grant would let a later `/hooks reload` or
/// new session run the new code with no trust decision (TOCTOU); and (2) the
/// unrecordable-key allow (cwd is $HOME / fs-root), which the store can never
/// persist anyway. Store-trusted, feature-off, and an accepted prompt are
/// durable; an untrusted verdict is recorded so a later `--trust` grant can
/// reconcile it (see [`resolve_and_record_inner`]).
fn compute(
    cwd: &Path,
    key: &Path,
    remote: Option<&RemoteSettings>,
    allow_prompt: bool,
) -> (bool, bool) {
    let feature = feature_enabled(remote);
    let inputs = decide_inputs(cwd, key);
    compute_from_inputs(&inputs, feature, key, allow_prompt)
}

/// [`compute`] split at the gather: derive `(allowed, durable)` from an
/// already-gathered [`DecideInputs`] so a caller needing more than one verdict
/// (see [`resolve_launch_dir_trust`]) pays for the expensive `decide_inputs`
/// gather (store read + `repo_configs_present` scan) only ONCE.
fn compute_from_inputs(
    inputs: &DecideInputs,
    feature: bool,
    key: &Path,
    allow_prompt: bool,
) -> (bool, bool) {
    match decide(feature, inputs) {
        TrustOutcome::Trusted => {
            // Within the Trusted arm the non-durable ("provisional") allows are the
            // "no repo configs" rule and the unrecordable-key rule (Case 2: cwd is
            // $HOME / fs-root, which can never be persisted) — both are feature-on
            // and not store-trusted; feature-off and store-trusted are durable.
            // Leave the non-durable allows uncached.
            let durable = !feature || inputs.store_trusted;
            (true, durable)
        }
        TrustOutcome::Prompt if allow_prompt => {
            if prompt_for_trust(key) {
                // Reload the store (the inputs gather dropped its copy) to
                // persist the accepted prompt grant.
                persist_trust(&mut TrustStore::load(), key);
                (true, true)
            } else {
                (false, true)
            }
        }
        // Untrusted, OR interactive where prompting is unsafe here (TUI owns
        // stdin) — the agent-`initialize` path owns the launch-dir prompt. Both
        // resolve fail-closed.
        TrustOutcome::Untrusted | TrustOutcome::Prompt => (false, true),
    }
}

/// PROJECT-scoped MCP server display names for `cwd` — the names dropped from a
/// merged server list when the workspace is untrusted.
///
/// SINGLE SOURCE OF TRUTH for "project-scoped MCP names" across ALL gate sites
/// (session merge, the session-less agent pool, `echoagent mcp doctor`). It MUST
/// enumerate every project MCP source the loaders read; adding a new repo-local
/// MCP source without extending this fn silently re-opens the gate (guarded by
/// `project_scoped_mcp_names_cover_every_source`).
///
/// Name-based (not `ConfigSource`-based) ON PURPOSE: it is the one primitive
/// that works for BOTH the sourced session merge AND the flat `load_mcp_servers`
/// agent-pool/doctor paths, which carry no `ConfigSource`. Names use the same
/// identity the merge dedups on ([`mcp_server_name`]).
///
/// Sources: project `.echo-agent/config.toml [mcp_servers]` (NOT the user-tier global
/// config), project `.mcp.json` (`cwd` up to the repo root, never `$HOME`),
/// project `.cursor/mcp.json`, and `~/.claude.json projects.<cwd>.mcpServers`.
///
/// Edge case: a name declared in BOTH a project config and the global
/// `~/.echo-agent/config.toml` is dropped when untrusted. This is intended — untrusted
/// project content must not influence the command spawned for a shared name.
pub(crate) fn project_scoped_mcp_names(cwd: &Path) -> HashSet<String> {
    let mut names = HashSet::new();

    // `.echo-agent/config.toml [mcp_servers]` entries tagged project (the loader's key
    // is the display name, matching `mcp_server_name` of the merged server).
    for (name, (_cfg, scope)) in crate::util::config::load_mcp_server_configs_with_project(cwd) {
        if scope == MCP_SCOPE_PROJECT {
            names.insert(name);
        }
    }

    // Project `.mcp.json` (repo-root-down; this loader never reads $HOME).
    for server in crate::util::config::load_mcp_json_servers(cwd) {
        names.insert(mcp_server_name(&server).to_string());
    }

    // Project `.cursor/mcp.json` only — `load_cursor_mcp_servers` would also
    // merge the user-scoped `~/.cursor/mcp.json`, so read the project file
    // directly instead.
    for server in crate::util::config::load_mcp_json_file(&cwd.join(".cursor").join("mcp.json")) {
        names.insert(mcp_server_name(&server).to_string());
    }

    // `~/.claude.json projects.<cwd>.mcpServers` object keys.
    if let Some(claude_names) = claude_project_mcp_names(cwd) {
        names.extend(claude_names);
    }

    names
}

/// Drop repo-local (project-scoped) MCP servers from a merged server list when
/// `cwd`'s workspace is untrusted. No-op when project scope is allowed. Mirrors
/// [`filter_untrusted_project_lsp`].
///
/// Matches on display name ([`mcp_server_name`]) — the same identity the merge
/// dedups and the disabled/allowlist filters use — rather than the URL/key, so a
/// project server is dropped regardless of transport. Because the match is by
/// name, a server from ANY tier (client/plugin/user/managed) whose name COLLIDES
/// with a project-declared name is ALSO dropped when untrusted: an untrusted repo
/// must not influence the command spawned for that name (see
/// [`project_scoped_mcp_names`]). Servers with no project-name collision are kept.
pub(crate) fn filter_untrusted_project_mcp(
    cwd: &Path,
    merged: Vec<acp::McpServer>,
) -> Vec<acp::McpServer> {
    if project_scope_allowed(cwd) {
        return merged;
    }
    let project = project_scoped_mcp_names(cwd);
    merged
        .into_iter()
        .filter(|server| {
            let name = mcp_server_name(server);
            if project.contains(name) {
                tracing::warn!(
                    server = %name,
                    "folder untrusted: skipping repo-local (project-scoped) MCP server"
                );
                false
            } else {
                true
            }
        })
        .collect()
}

/// Drop repo-local (project-scoped) LSP servers from a sourced LSP map when
/// `cwd`'s workspace is untrusted. Mirrors the MCP session gate; user- and
/// plugin-scoped servers are retained. No-op when project scope is allowed.
///
/// Thin `cwd`→verdict wrapper over the shared
/// [`echo_agent_tools::implementations::lsp::config::filter_project_lsp_when_untrusted`]
/// predicate, so Site B and the workspace build path share one gate.
pub(crate) fn filter_untrusted_project_lsp(
    cwd: &Path,
    sourced: std::collections::BTreeMap<
        String,
        (
            echo_agent_tools::implementations::lsp::config::LspServerConfig,
            echo_agent_tools::types::config_source::ConfigSource,
        ),
    >,
) -> std::collections::BTreeMap<
    String,
    echo_agent_tools::implementations::lsp::config::LspServerConfig,
> {
    echo_agent_tools::implementations::lsp::config::filter_project_lsp_when_untrusted(
        sourced,
        project_scope_allowed(cwd),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // Used only by the consume-side regression test below; imported here (not at
    // module scope) so the non-test build doesn't carry an unused import.
    use echo_agent_workspace::folder_trust::repo_configs_present;

    /// A `git init`'d temp dir so `find_mcp_json_files` / `find_project_configs`
    /// (which discover the enclosing repo and walk to its root) are bounded to
    /// the temp dir instead of any ancestor repo the system temp dir lives in.
    fn repo_tmp() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        tmp
    }

    /// Simulate a release-stamped build so the folder-trust gate engages: an
    /// unstamped local/dev build auto-trusts and never gates/persists. Hold the
    /// returned guard for the test body (drop restores the prior value).
    fn simulate_release_build() -> EnvGuard {
        EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0.0-sim")
    }

    #[test]
    fn record_and_lookup_round_trip() {
        // `repo_tmp` git-inits the dir so `workspace_key` yields a unique key
        // (it returns the git-repo root, which would otherwise collapse to a
        // shared ambient root if `$TMPDIR` is inside a checkout). The
        // `DECISIONS` map is process-global, so unique keys keep parallel tests
        // from clobbering each other's recorded decisions.
        let tmp = repo_tmp();
        let key = tmp.path().to_path_buf();
        // A fresh, never-recorded key re-resolves fail-closed and is allowed here
        // (inert local build / no repo configs — never a durable default-open).
        assert!(project_scope_allowed(&key));
        record(&workspace_key(&key), false);
        assert!(!project_scope_allowed(&key));
    }

    #[test]
    #[serial_test::serial]
    fn revoke_folder_trust_downgrades_cache() {
        let _sim = simulate_release_build();
        // A mid-session untrust of a TRUSTED folder must take effect immediately:
        // revoke downgrades the in-process cache so `project_scope_allowed` flips
        // to false at once (a cached grant would otherwise short-circuit
        // `resolve_and_record`). Seed the trust store so `was_trusted` is genuinely
        // true; ECHO_AGENT_HOME-isolated so the seed can't touch the real user file and
        // `#[serial]` because ECHO_AGENT_HOME is process-global.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let tmp = repo_tmp();
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(tmp.path())).unwrap();
        record(&workspace_key(tmp.path()), true);
        assert!(project_scope_allowed(tmp.path()));
        assert!(
            revoke_folder_trust(tmp.path()),
            "a store-trusted folder must report was_trusted=true"
        );
        assert!(
            !project_scope_allowed(tmp.path()),
            "cache must be downgraded so untrust is immediate"
        );
    }

    #[test]
    #[serial_test::serial]
    fn revoke_never_trusted_folder_writes_no_deny_and_preserves_cascade() {
        let _sim = simulate_release_build();
        // The actual bug fix: revoking a NEVER-trusted child must write no explicit
        // child STORE deny (returning false) so a later ancestor grant still
        // cascades to the child — a spurious child `set_untrusted` would win
        // most-specific and break the cascade. It must STILL downgrade the
        // in-process cache, though, so a cached storeless grant cannot survive a
        // mid-session untrust. ECHO_AGENT_HOME-isolated so the grant writes to a temp
        // store; `#[serial]` because ECHO_AGENT_HOME is global.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        // Distinct git roots for parent/child so `workspace_key` does not collapse
        // them onto one key (the child's own `.git` stops discovery at the child).
        let parent = repo_tmp();
        let child = parent.path().join("child");
        std::fs::create_dir_all(&child).unwrap();
        git2::Repository::init(&child).unwrap();

        // Seed a cached grant with NO backing store record (e.g. a kill-switch /
        // feature-off `compute` allow), so the cache downgrade is observable below.
        record(&workspace_key(&child), true);

        // Revoking the never-trusted child reports false and writes no store deny.
        assert!(
            !revoke_folder_trust(&child),
            "revoking a never-trusted folder must return false"
        );

        // The in-process cache is still downgraded so the untrust is immediate even
        // for the storeless cached grant.
        assert!(
            !project_scope_allowed(&child),
            "revoke must downgrade the in-process cache even for a never-trusted folder"
        );

        // A subsequent ancestor grant must still cascade to the child — proving
        // the revoke did not poison the store with a most-specific child deny.
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(parent.path())).unwrap();
        assert!(
            TrustStore::load().is_trusted(&workspace_key(&child)),
            "ancestor grant must cascade to a child that was only revoked-when-untrusted"
        );
    }

    #[test]
    #[serial_test::serial]
    fn revoke_ancestor_cascade_trusted_child_writes_explicit_untrust() {
        let _sim = simulate_release_build();
        // Revoke on a child trusted ONLY via an ancestor cascade (no direct child
        // grant) must report was_trusted=true and actually untrust the child: it
        // writes an explicit child deny (overriding the cascade) and downgrades
        // the cache. ECHO_AGENT_HOME-isolated so the grant writes to a temp store;
        // `#[serial]` because ECHO_AGENT_HOME is process-global.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        // Distinct git roots so `workspace_key` keeps parent/child as separate
        // keys (the child's own `.git` stops discovery at the child).
        let parent = repo_tmp();
        let child = parent.path().join("child");
        std::fs::create_dir_all(&child).unwrap();
        git2::Repository::init(&child).unwrap();

        // Trust the parent only; the child inherits trust via the cascade.
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(parent.path())).unwrap();
        assert!(
            TrustStore::load().is_trusted(&workspace_key(&child)),
            "child must be trusted via the ancestor cascade before revoke"
        );

        // Revoking the cascade-trusted child reports was_trusted=true, then the
        // child is untrusted: an explicit child deny overrides the cascade and
        // the in-process cache is downgraded.
        assert!(
            revoke_folder_trust(&child),
            "a cascade-trusted child must report was_trusted=true"
        );
        assert!(
            !TrustStore::load().is_trusted(&workspace_key(&child)),
            "revoke must write an explicit child untrust that overrides the cascade"
        );
        assert!(
            !project_scope_allowed(&child),
            "revoke must downgrade the in-process cache for the child"
        );
    }

    #[test]
    #[serial_test::serial]
    fn revoke_on_unrecordable_home_root_records_no_deny() {
        let _sim = simulate_release_build();
        // cwd == $HOME (git-inited so `workspace_key` resolves the home root,
        // which the store refuses to record): revoke must NOT seed a cache deny.
        // decide() always trusts an unrecordable root and no grant/store/prompt
        // could ever lift the deny, so the gate must keep allowing after an
        // untrust click. HOME overridden so workspace_key sees the tempdir as
        // home; ECHO_AGENT_HOME-isolated store; ECHO_AGENT_FOLDER_TRUST unset so the
        // default-on flag applies.
        let home = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", home.path());
        let echo_agent_home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", echo_agent_home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        git2::Repository::init(home.path()).unwrap();
        // Repo-local code-exec config, so the final allow is the unrecordable-key
        // rule at work (a recordable key with configs + empty store would deny).
        std::fs::create_dir_all(home.path().join(".echo-agent").join("hooks")).unwrap();

        assert!(
            !revoke_folder_trust(home.path()),
            "an unrecordable root reports was_trusted=false"
        );
        assert!(
            DECISIONS.lock().get(&workspace_key(home.path())).is_none(),
            "revoke must record no cache deny for an unrecordable root"
        );
        assert!(
            project_scope_allowed(home.path()),
            "$HOME must stay allowed after a revoke attempt"
        );
    }

    #[test]
    #[serial_test::serial]
    fn envrc_gate_drops_untrusted_then_loads_when_store_trusted() {
        let _sim = simulate_release_build();
        // The `.envrc` load sites gate on the folder-trust verdict: an
        // `.envrc`-only untrusted clone resolves false (so the call site loads an
        // empty env), while a store-trusted folder resolves true and the loader
        // actually reads `.envrc`. ECHO_AGENT_HOME-isolated so the trust store is empty;
        // ECHO_AGENT_FOLDER_TRUST unset so the default-on feature flag applies.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        std::fs::write(tmp.path().join(".envrc"), "export GATED_ENVRC=1\n").unwrap();

        // Untrusted: the gate verdict is false => the call site skips loading.
        assert!(
            !resolve_and_record(tmp.path(), None, false),
            "untrusted `.envrc` clone must gate the load"
        );

        // Store-trust the folder => the verdict reconciles to true and the loader runs.
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(tmp.path())).unwrap();
        assert!(resolve_and_record(tmp.path(), None, false));
        let env = echo_agent_workspace::envrc::load_envrc_or_empty(tmp.path());
        assert_eq!(
            env.get("GATED_ENVRC"),
            Some(&"1".to_string()),
            "trusted folder must load `.envrc`"
        );
    }

    #[test]
    #[serial_test::serial]
    fn claude_env_gate_drops_project_env_when_untrusted() {
        let _sim = simulate_release_build();
        // The `.claude/settings.json` env load site mirrors
        // `load_claude_env_with_project(cwd, project_scope_allowed(cwd))`: an
        // untrusted clone's repo-tree env (which would feed BASH_ENV /
        // GIT_SSH_COMMAND / … to every subprocess) is dropped; a store-trusted
        // folder merges it. ECHO_AGENT_HOME-isolated so the trust store is empty;
        // ECHO_AGENT_FOLDER_TRUST unset so the default-on feature flag applies.
        use echo_agent_workspace::permission::claude_settings::load_claude_env_with_project;
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        let claude = tmp.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            claude.join("settings.json"),
            r#"{"env": {"REPO_TREE_ENV_GATED": "1"}}"#,
        )
        .unwrap();

        // Untrusted: verdict false => the repo-tree env is dropped.
        assert!(!resolve_and_record(tmp.path(), None, false));
        let untrusted =
            load_claude_env_with_project(tmp.path(), resolve_and_record(tmp.path(), None, false));
        assert!(
            !untrusted.contains_key("REPO_TREE_ENV_GATED"),
            "untrusted folder must drop repo-tree .claude env"
        );

        // Store-trust => verdict reconciles to true => repo-tree env merged.
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(tmp.path())).unwrap();
        assert!(resolve_and_record(tmp.path(), None, false));
        let trusted =
            load_claude_env_with_project(tmp.path(), resolve_and_record(tmp.path(), None, false));
        assert_eq!(
            trusted.get("REPO_TREE_ENV_GATED"),
            Some(&"1".to_string()),
            "trusted folder must merge repo-tree .claude env"
        );
    }

    #[test]
    #[serial_test::serial]
    fn claude_env_gate_drops_subdir_project_env_when_untrusted() {
        let _sim = simulate_release_build();
        // RCE regression (subdir bypass): a `.claude/settings.json` with `env` in
        // a SUBDIR — the ONLY repo config — launched from that subdir must flip the
        // folder untrusted AND have its env dropped. The env loader walks
        // cwd→repo-root, so detection MUST walk too (a git-root-only probe missed
        // this). ECHO_AGENT_HOME-isolated so the trust store is empty.
        use echo_agent_workspace::permission::claude_settings::load_claude_env_with_project;
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        let subdir = tmp.path().join("sub");
        let claude = subdir.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            claude.join("settings.json"),
            r#"{"env": {"SUBDIR_REPO_ENV_GATED": "1"}}"#,
        )
        .unwrap();

        // Detection now walks cwd→root, so the subdir-only `.claude` is detected
        // and the folder resolves untrusted.
        assert!(
            repo_configs_present(&subdir),
            "subdir .claude/settings.json must be detected"
        );
        assert!(!resolve_and_record(&subdir, None, false));
        let untrusted =
            load_claude_env_with_project(&subdir, resolve_and_record(&subdir, None, false));
        assert!(
            !untrusted.contains_key("SUBDIR_REPO_ENV_GATED"),
            "untrusted subdir folder must drop its repo-tree .claude env"
        );

        // Granting trust re-enables it (proves the env file is real/loadable).
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(&subdir)).unwrap();
        assert!(resolve_and_record(&subdir, None, false));
        let trusted =
            load_claude_env_with_project(&subdir, resolve_and_record(&subdir, None, false));
        assert_eq!(
            trusted.get("SUBDIR_REPO_ENV_GATED"),
            Some(&"1".to_string()),
            "trusted folder must merge the subdir repo-tree .claude env"
        );
    }

    #[test]
    #[serial_test::serial]
    fn project_agent_inline_hooks_gated_when_untrusted_but_user_kept() {
        let _sim = simulate_release_build();
        // A cwd-discovered PROJECT agent's inline `hooks:` is gated on folder-trust
        // (it can SHADOW a built-in subagent => near-auto RCE); a user/built-in
        // agent's hooks are kept. Exercises real discovery + the exact call-site
        // predicate used at mvp_agent/subagent. ECHO_AGENT_HOME-isolated (empty store).
        use echo_agent_core::config::AgentScope;
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        let agents = tmp.path().join(".echo-agent").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        // Shadows the built-in `explore` subagent and carries a command hook.
        std::fs::write(
            agents.join("explore.md"),
            "---\nname: explore\ndescription: x\nhooks:\n  PreToolUse:\n    - hooks:\n        - type: command\n          command: \"true\"\n---\nbody\n",
        )
        .unwrap();
        let def = echo_agent_core::discovery::by_name_in_cwd("explore", tmp.path())
            .expect("project agent must be discovered");
        assert_eq!(def.scope, AgentScope::Project);
        assert!(def.hooks.is_some(), "project agent must carry inline hooks");

        // Exercise the REAL shared predicate both append sites use (not a copy).
        let allowed = |scope: AgentScope| {
            agent_inline_hooks_allowed(scope, || project_scope_allowed(tmp.path()))
        };

        // Untrusted: the project agent's inline hooks are dropped...
        assert!(
            !allowed(def.scope),
            "untrusted project agent inline hooks must be gated"
        );
        // ...while user/built-in agents (not cwd-sourced) keep their hooks.
        assert!(allowed(AgentScope::User));
        assert!(allowed(AgentScope::BuiltIn));

        // Grant trust + reconcile the cache (as the post-grant reload flow does)
        // => the project agent's inline hooks are appended.
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(tmp.path())).unwrap();
        resolve_and_record(tmp.path(), None, false);
        assert!(
            allowed(def.scope),
            "trusted project agent inline hooks must be appended"
        );
    }

    use echo_agent_test_support::EnvGuard;

    #[test]
    #[serial_test::serial]
    fn project_scope_allowed_denies_untrusted_repo_with_configs() {
        // Fail-closed (the dangerous case): a release-stamped build with the
        // feature on by default, an untrusted folder that ships repo-local
        // code-exec config (here `.echo-agent/hooks`), and no store grant must be
        // DENIED — even though no verdict was recorded first (the gate re-resolves
        // fail-closed rather than defaulting open). ECHO_AGENT_HOME-isolated (empty
        // store); ECHO_AGENT_FOLDER_TRUST unset so the default-on flag applies.
        let _sim = simulate_release_build();
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("hooks")).unwrap();
        assert!(
            !project_scope_allowed(tmp.path()),
            "untrusted folder with repo configs must be denied (fail-closed)"
        );
    }

    #[test]
    #[serial_test::serial]
    fn project_scope_allowed_allows_repo_without_configs() {
        // The over-deny guard: a folder with NO repo-local code-exec config has
        // nothing to gate, so it must be ALLOWED even though its (provisional)
        // Trusted verdict is never cached — a naive `.unwrap_or(false)` cache peek
        // would wrongly deny it. Release-stamped + ECHO_AGENT_HOME-isolated so the
        // verdict comes from `decide` rule 4 (no repo configs), not the inert
        // short-circuit.
        let _sim = simulate_release_build();
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        assert!(
            project_scope_allowed(tmp.path()),
            "a folder with no repo configs must be allowed (no over-deny)"
        );
    }

    #[test]
    #[serial_test::serial]
    fn project_scope_allowed_allows_store_trusted_repo() {
        // A folder the user explicitly trusted is ALLOWED even with repo-local
        // configs present. ECHO_AGENT_HOME-isolated so the seeded store is the temp one;
        // ECHO_AGENT_FOLDER_TRUST unset so the default-on flag applies.
        let _sim = simulate_release_build();
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("hooks")).unwrap();
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(tmp.path())).unwrap();
        assert!(
            project_scope_allowed(tmp.path()),
            "a store-trusted folder must be allowed"
        );
    }

    #[test]
    #[serial_test::serial]
    fn project_scope_allowed_allows_inert_local_build() {
        // On a local/dev build the whole feature is inert (auto-trust): a folder
        // with repo-local configs and an empty store is still ALLOWED. Assert only
        // when compiled unstamped (mirrors the inert tests elsewhere), with
        // ECHO_AGENT_TEST_VERSION unset so `is_local_build()` is genuinely true.
        let _unset_ver = EnvGuard::unset(echo_agent_version::TEST_VERSION_ENV);
        if option_env!("ECHO_AGENT_VERSION").is_some() {
            return; // a release-stamped test binary is not a local build
        }
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("hooks")).unwrap();
        assert!(
            project_scope_allowed(tmp.path()),
            "inert local/dev build must allow project scope even with configs"
        );
    }

    #[test]
    #[serial_test::serial]
    fn project_scope_allowed_denies_untrusted_plugin_only_repo() {
        // A plugin-only untrusted repo (just `.echo-agent/plugins/<x>/`, no
        // hooks/MCP/LSP, no store grant) is repo-controlled code-exec and must be
        // DENIED — the verdict the shell plugin call sites feed into
        // discover_plugins/build_for_cwd/reload. ECHO_AGENT_HOME-isolated (empty store);
        // ECHO_AGENT_FOLDER_TRUST unset so the default-on flag applies.
        let _sim = simulate_release_build();
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("plugins").join("evil"))
            .unwrap();
        assert!(
            !project_scope_allowed(tmp.path()),
            "plugin-only untrusted repo must be denied"
        );
    }

    #[test]
    #[serial_test::serial]
    fn project_scope_allowed_denies_untrusted_permission_only_repo() {
        // Bridge: a clone whose ONLY repo-local config is `.echo-agent/config.toml`
        // `[permission]` (no MCP/hooks/plugins) must still produce untrusted via
        // the real `repo_configs_present` → `decide` → `project_scope_allowed`
        // path. Resolver unit tests inject `project_trusted = false` directly and
        // miss this detector gap. Subdir launch ensures the cwd→git-root walk.
        let _sim = simulate_release_build();
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            "[permission]\nallow = [\"Bash(*)\"]\n",
        )
        .unwrap();
        let subdir = tmp.path().join("crates").join("inner");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(
            !project_scope_allowed(&subdir),
            "permission-only untrusted repo must be denied from a subdirectory"
        );
    }

    #[test]
    #[serial_test::serial]
    fn kill_switch_allows_untrusted_repo_after_authoritative_resolve() {
        // Regression (chat/load-path kill-switch): an untrusted folder WITH repo
        // configs under a remote kill-switch (folder_trust_enabled = Some(false))
        // must resolve ALLOWED. The session spawn path resolves once with the real
        // RemoteSettings before any gate read, so the gate cache-hits that verdict.
        // ECHO_AGENT_HOME-isolated (empty store); ECHO_AGENT_FOLDER_TRUST unset so the kill-switch
        // is the only signal.
        let _sim = simulate_release_build();
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let remote = RemoteSettings {
            folder_trust_enabled: Some(false),
            ..Default::default()
        };

        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("hooks")).unwrap();
        assert!(
            resolve_and_record(tmp.path(), Some(&remote), false),
            "kill-switch (feature off) must resolve trusted even with repo configs"
        );
        assert!(
            project_scope_allowed(tmp.path()),
            "gate must allow a kill-switched folder once the authoritative resolve ran"
        );

        // Contrast: a cold `remote = None` gate read (no prior authoritative resolve)
        // misses the kill-switch and denies the same scenario — the exact gap the
        // up-front spawn resolve closes for chat/load sessions.
        let cold = repo_tmp();
        std::fs::create_dir_all(cold.path().join(".echo-agent").join("hooks")).unwrap();
        assert!(
            !project_scope_allowed(cold.path()),
            "cold remote=None gate read denies a kill-switched folder (regression contrast)"
        );
    }

    #[test]
    #[serial_test::serial]
    fn build_for_cwd_with_trust_verdict_gates_active_project_plugin() {
        let _sim = simulate_release_build();
        // Pins the SHELL plugin wiring end-to-end: the call-site expression
        // `build_for_cwd(cwd, &cfg, dirs, <folder-trust verdict>)` must keep an
        // ENABLED project plugin OUT of `active_plugins()` while the folder is
        // untrusted, and let it in after `grant_folder_trust`. The
        // verdict/discovery/registry unit tests alone do NOT catch a silent
        // un-gating here.
        //
        // ECHO_AGENT_HOME-isolated so both the folder-trust store and the plugin trust
        // store start empty (deterministic untrusted); ECHO_AGENT_FOLDER_TRUST unset so
        // the default-on flag applies; `#[serial]` because both are process-global.
        use echo_agent_core::plugins::discovery::DiscoveryConfig;
        use echo_agent_core::plugins::{PluginRegistry, SharedPluginRegistryHandle};
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        // A project plugin. Project scope is default-disabled, so name it in the
        // `enabled` list to isolate the TRUST gate (not the enable gate).
        let plugin = tmp
            .path()
            .join(".echo-agent")
            .join("plugins")
            .join("trustgate");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"name":"trustgate"}"#).unwrap();
        let cfg = DiscoveryConfig {
            enabled: vec!["trustgate".to_string()],
            ..Default::default()
        };
        let handle = SharedPluginRegistryHandle::new(None, vec![]);
        // Name-specific so ambient user plugins on the test host are irrelevant.
        let active_has = |reg: Option<std::sync::Arc<PluginRegistry>>| {
            reg.is_some_and(|r| r.active_plugins().iter().any(|p| p.name == "trustgate"))
        };

        // Untrusted: the verdict is false => the plugin is still discovered but
        // not trusted, so it is absent from `active_plugins()`.
        let untrusted = handle.build_for_cwd(
            tmp.path(),
            &cfg,
            &[],
            resolve_and_record(tmp.path(), None, false),
        );
        assert!(
            !active_has(untrusted),
            "untrusted folder: enabled project plugin must be gated out of active_plugins()"
        );

        // Grant trust, recompute the same expression => the plugin becomes active.
        grant_folder_trust(tmp.path());
        let trusted = handle.build_for_cwd(
            tmp.path(),
            &cfg,
            &[],
            resolve_and_record(tmp.path(), None, false),
        );
        assert!(
            active_has(trusted),
            "granted folder: enabled project plugin must appear in active_plugins()"
        );
    }

    #[test]
    #[serial_test::serial]
    fn discover_hooks_gates_then_loads_project_hook_via_trust_verdict() {
        let _sim = simulate_release_build();
        // End-to-end load path: the folder-trust verdict threaded into `discover_hooks`
        // excludes a repo-local project hook while untrusted, and includes it after the
        // folder is granted trust — the path where the regression historically re-opened.
        // ECHO_AGENT_HOME-isolated so the grant writes to a temp store; ECHO_AGENT_FOLDER_TRUST unset
        // so the default-on flag applies.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let tmp = repo_tmp();
        let hooks_dir = tmp.path().join(".echo-agent").join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        // Top-level `{"hooks":{...}}` wrapper; no matcher => match-all. The parsed
        // spec name is `<file_stem>:PreToolUse[..]`, so the file stem identifies it.
        std::fs::write(
            hooks_dir.join("trust_load_gate.json"),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
        )
        .unwrap();
        // Discovery prefixes project specs with `project/` and parse names them
        // `<file_stem>:<event>[..]`, so the unique stem appears mid-name;
        // `contains` matches it without coupling to the full name format.
        let has_project_hook = |reg: &echo_agent_hooks::discovery::HookRegistry| {
            reg.all_hooks()
                .iter()
                .any(|h| h.name.contains("trust_load_gate"))
        };

        // Untrusted: verdict false => discovery omits the project hook.
        let untrusted = resolve_and_record(tmp.path(), None, false);
        assert!(!untrusted, "untrusted repo must resolve the gate false");
        // Mirror the production startup/reload path via the single load entry point.
        let git_root = echo_agent_workspace::session::git::find_git_root_from_path(tmp.path()).ok();
        let (reg, _errs) = crate::util::hooks::discover_hooks(
            git_root.as_deref(),
            &echo_agent_tools::types::compat::CompatConfig::default(),
            untrusted,
        );
        assert!(
            !has_project_hook(&reg),
            "untrusted: repo-local project hook must be gated out of discovery"
        );

        // Grant trust, re-resolve => discovery loads the project hook.
        grant_folder_trust(tmp.path());
        let trusted = resolve_and_record(tmp.path(), None, false);
        assert!(trusted, "granted repo must resolve the gate true");
        let (reg, _errs) = crate::util::hooks::discover_hooks(
            git_root.as_deref(),
            &echo_agent_tools::types::compat::CompatConfig::default(),
            trusted,
        );
        assert!(
            has_project_hook(&reg),
            "trusted: repo-local project hook must load"
        );
    }

    #[test]
    fn filter_untrusted_project_lsp_drops_only_project() {
        use echo_agent_tools::implementations::lsp::config::LspServerConfig;
        use echo_agent_tools::types::config_source::ConfigSource;
        use std::collections::BTreeMap;

        fn sourced() -> BTreeMap<String, (LspServerConfig, ConfigSource)> {
            let mut m = BTreeMap::new();
            m.insert(
                "proj".to_string(),
                (
                    LspServerConfig::default(),
                    ConfigSource::Project {
                        path: PathBuf::from("/repo/.echo-agent/lsp.json"),
                    },
                ),
            );
            m.insert(
                "usr".to_string(),
                (
                    LspServerConfig::default(),
                    ConfigSource::User {
                        path: PathBuf::from("/home/.echo-agent/lsp.json"),
                    },
                ),
            );
            m
        }

        // Untrusted workspace: only the project-scoped server is dropped.
        // `repo_tmp` git-inits so `workspace_key` yields a unique per-dir key
        // even when `$TMPDIR` itself lives inside a git checkout.
        let untrusted = repo_tmp();
        record_for_test(untrusted.path(), false);
        let kept = filter_untrusted_project_lsp(untrusted.path(), sourced());
        assert_eq!(kept.len(), 1);
        assert!(kept.contains_key("usr"));
        assert!(!kept.contains_key("proj"));

        // Trusted workspace (a different temp-dir key): both are retained.
        let trusted = repo_tmp();
        record_for_test(trusted.path(), true);
        let kept = filter_untrusted_project_lsp(trusted.path(), sourced());
        assert_eq!(kept.len(), 2);
        assert!(kept.contains_key("usr"));
        assert!(kept.contains_key("proj"));
    }

    #[test]
    fn load_servers_sourced_tags_project_lsp_json() {
        use echo_agent_tools::implementations::lsp::config::load_servers_with_plugins_sourced;
        use echo_agent_tools::types::config_source::ConfigSource;

        // A `<cwd>/.echo-agent/lsp.json` server must be tagged `Project` so the gate
        // can distinguish it from user/plugin servers. Asserts on the specific
        // key, so any real `~/.echo-agent/lsp.json` on the test host is irrelevant.
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("lsp.json"),
            r#"{"projlsp": {"command": "true"}}"#,
        )
        .unwrap();

        let sourced = load_servers_with_plugins_sourced(tmp.path(), &[], &[], &[], &[]);
        let (_, source) = sourced.get("projlsp").expect("project server present");
        assert!(
            matches!(source, ConfigSource::Project { .. }),
            "project lsp.json must be tagged Project; got {source:?}"
        );
    }

    #[test]
    fn untrusted_workspace_drops_loaded_project_lsp() {
        use echo_agent_tools::implementations::lsp::config::load_servers_with_plugins_sourced;

        // End-to-end of the load-site gate (Sites A/B): a project server loaded
        // from `<cwd>/.echo-agent/lsp.json` is dropped once the workspace is untrusted.
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("lsp.json"),
            r#"{"projlsp": {"command": "true"}}"#,
        )
        .unwrap();

        let sourced = load_servers_with_plugins_sourced(tmp.path(), &[], &[], &[], &[]);
        assert!(
            sourced.contains_key("projlsp"),
            "loader must see the project server"
        );

        record_for_test(tmp.path(), false);
        let kept = filter_untrusted_project_lsp(tmp.path(), sourced);
        assert!(
            !kept.contains_key("projlsp"),
            "untrusted folder must drop the project-scoped LSP server"
        );
    }

    fn http(name: &str) -> acp::McpServer {
        acp::McpServer::Http(
            acp::McpServerHttp::new(name.to_string(), format!("https://example.com/{name}"))
                .headers(vec![]),
        )
    }

    /// A git-init'd repo declaring two project-scoped MCP servers: `projjson`
    /// (`.mcp.json`) and `projtoml` (`.echo-agent/config.toml [mcp_servers]`).
    fn repo_with_project_mcp() -> tempfile::TempDir {
        let tmp = repo_tmp();
        std::fs::write(
            tmp.path().join(".mcp.json"),
            r#"{"mcpServers": {"projjson": {"url": "https://proj.example.com/mcp"}}}"#,
        )
        .unwrap();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            "[mcp_servers.projtoml]\nurl = \"https://projtoml.example.com/mcp\"\n",
        )
        .unwrap();
        tmp
    }

    /// Pins the three known repo-local FILE sources of
    /// [`project_scoped_mcp_names`]: a project server declared in each of
    /// `.echo-agent/config.toml`, `.mcp.json`, and `.cursor/mcp.json` must appear in
    /// the returned set, catching a REGRESSION that drops one of them. It cannot
    /// catch a brand-new source TYPE added only to a loader — the prominent
    /// single-source-of-truth doc on `project_scoped_mcp_names` is that guard.
    /// (`~/.claude.json` is excluded — it lives under `$HOME` and a test must not
    /// clobber the real user file; its keys are covered by the shared reader.)
    #[test]
    fn project_scoped_mcp_names_cover_every_source() {
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            "[mcp_servers.cfgsrv]\nurl = \"https://cfg.example.com/mcp\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join(".mcp.json"),
            r#"{"mcpServers": {"jsonsrv": {"url": "https://json.example.com/mcp"}}}"#,
        )
        .unwrap();
        let cursor = tmp.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(
            cursor.join("mcp.json"),
            r#"{"mcpServers": {"cursorsrv": {"url": "https://cursor.example.com/mcp"}}}"#,
        )
        .unwrap();

        let names = project_scoped_mcp_names(tmp.path());
        for expected in ["cfgsrv", "jsonsrv", "cursorsrv"] {
            assert!(
                names.contains(expected),
                "project_scoped_mcp_names missing {expected:?} — a project MCP source is no longer gated; got {names:?}"
            );
        }
    }

    #[test]
    fn filter_untrusted_project_mcp_drops_only_project() {
        let merged = || {
            vec![
                http("projjson"),
                http("projtoml"),
                http("client"),
                http("global"),
            ]
        };

        // Untrusted: both project-declared servers are dropped; a client-supplied
        // and a user/global server (neither in a project config) are retained.
        let untrusted = repo_with_project_mcp();
        record_for_test(untrusted.path(), false);
        let kept = filter_untrusted_project_mcp(untrusted.path(), merged());
        let names: HashSet<&str> = kept.iter().map(mcp_server_name).collect();
        assert_eq!(names, ["client", "global"].into_iter().collect());

        // Trusted: every server is retained even with project configs present.
        let trusted = repo_with_project_mcp();
        record_for_test(trusted.path(), true);
        let kept = filter_untrusted_project_mcp(trusted.path(), merged());
        assert_eq!(kept.len(), 4);
    }

    #[test]
    fn cached_grant_short_circuits_without_store_read() {
        let tmp = repo_tmp();
        let key = workspace_key(tmp.path());
        record(&key, true);
        // A cached grant must consult neither the store nor a recompute.
        let allowed = resolve_and_record_inner(
            &key,
            || panic!("store must not be read for a cached grant"),
            || panic!("must not recompute for a cached grant"),
        );
        assert!(allowed);
    }

    #[test]
    fn cached_untrusted_is_upgraded_by_a_later_grant() {
        let tmp = repo_tmp();
        let key = workspace_key(tmp.path());
        record(&key, false);
        assert!(!project_scope_allowed(tmp.path()));
        // Simulate a `echoagent --trust` grant landing in the store after the
        // untrusted verdict was cached: the re-read sees trusted, so the next
        // resolve upgrades the cache without a process restart.
        let allowed = resolve_and_record_inner(
            &key,
            || true,
            || panic!("must not recompute when reconciling from the store"),
        );
        assert!(allowed);
        // The cheap gate the merge consults now allows (cache refreshed).
        assert!(project_scope_allowed(tmp.path()));
    }

    #[test]
    fn cached_untrusted_stays_untrusted_when_store_still_denies() {
        let tmp = repo_tmp();
        let key = workspace_key(tmp.path());
        record(&key, false);
        let allowed =
            resolve_and_record_inner(&key, || false, || panic!("must not recompute when cached"));
        assert!(!allowed);
        assert!(!project_scope_allowed(tmp.path()));
    }

    #[test]
    fn unrecorded_key_recomputes_then_caches() {
        let tmp = repo_tmp();
        let key = workspace_key(tmp.path());
        // No prior record → recompute and record the (durable untrusted) verdict.
        let allowed = resolve_and_record_inner(
            &key,
            || panic!("store re-read is only for a cached untrusted verdict"),
            || (false, true),
        );
        assert!(!allowed);
        assert!(!project_scope_allowed(tmp.path()));
        // Now cached untrusted: a later resolve re-reads the store (still denies).
        let allowed =
            resolve_and_record_inner(&key, || false, || panic!("must not recompute when cached"));
        assert!(!allowed);
    }

    #[test]
    fn explicit_child_untrust_survives_reload_despite_ancestor_trust() {
        // Untrust-undone-on-reload: an ancestor is trusted and the child is
        // explicitly untrusted, so a reload must NOT re-promote the child. revoke
        // downgrades the cache to untrusted; the reconcile re-reads the store,
        // which now honors the most-specific (child) decision and stays untrusted
        // — the ancestor's cascade no longer undoes the explicit untrust.
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp
            .path()
            .join(echo_agent_workspace::trust::TRUST_FILE_NAME);
        let parent = tmp.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();

        let mut store = TrustStore::load_from(store_path);
        store.set_trusted(&parent).unwrap();
        store.set_untrusted(&child).unwrap();

        // revoke downgraded the in-process cache for the child.
        record(&child, false);

        let allowed = resolve_and_record_inner(
            &child,
            || store.is_trusted(&child),
            || panic!("must not recompute when reconciling a cached untrusted verdict"),
        );
        assert!(
            !allowed,
            "explicit child untrust must survive the ancestor's trust on reload"
        );
    }

    #[test]
    #[serial_test::serial]
    fn no_configs_trust_is_provisional_and_regated_when_configs_appear() {
        // F5 regression: the "no repo configs => Trusted" verdict is PROVISIONAL,
        // so it must NOT be cached as a durable grant. Otherwise a clone that is
        // empty when first resolved, then gains a code-exec config (git pull /
        // agent write), would short-circuit the stale grant and load+run the new
        // hooks/plugins ungated on the next /hooks reload or new session (TOCTOU).
        //
        // Drives the real `resolve_and_record` + `project_scope_allowed`. Force
        // the feature on via env (highest precedence) so the test does not depend
        // on the host's folder-trust config.
        unsafe { std::env::set_var("ECHO_AGENT_FOLDER_TRUST", "1") };
        // Simulate a release-stamped build: an unstamped local/dev build (as in CI,
        // no ECHO_AGENT_VERSION) auto-trusts, so the gate would never engage without this.
        unsafe { std::env::set_var(echo_agent_version::TEST_VERSION_ENV, "0.0.0-sim") };
        let tmp = repo_tmp();

        // Empty repo: nothing to gate => allowed, but left UNRECORDED (provisional).
        assert!(resolve_and_record(tmp.path(), None, false));
        assert!(
            DECISIONS.lock().get(&workspace_key(tmp.path())).is_none(),
            "provisional no-configs allow must not be cached as a durable grant"
        );

        // A repo-local code-exec config appears after the first resolve.
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("hooks")).unwrap();

        // The next resolve re-checks `repo_configs_present` (no stale grant to
        // ride) => headless untrusted, so the newly-added hooks are now gated.
        assert!(
            !resolve_and_record(tmp.path(), None, false),
            "configs that appear after the first resolve must be re-checked and gated"
        );
        assert!(!project_scope_allowed(tmp.path()));

        unsafe { std::env::remove_var(echo_agent_version::TEST_VERSION_ENV) };
        unsafe { std::env::remove_var("ECHO_AGENT_FOLDER_TRUST") };
    }

    #[test]
    #[serial_test::serial]
    fn resolve_launch_dir_trust_matches_resolve_and_record() {
        // `resolve_launch_dir_trust` derives the launch-dir verdict from one
        // gather, so it must agree with `resolve_and_record(cwd, None, false)` and
        // must leave the provisional no-configs grant UNCACHED (the TOCTOU
        // contract on the shared path). Force the gate on via env (highest
        // precedence) so the test does not depend on the host config; isolate
        // ECHO_AGENT_HOME so the store is empty/seeded in temp. `#[serial]` because both
        // vars are process-global.
        let _feature = EnvGuard::set("ECHO_AGENT_FOLDER_TRUST", "1");
        let _sim = simulate_release_build();
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());

        // (a) No configs => provisional Trusted, NOT cached by the shared path.
        let empty = repo_tmp();
        let lt = resolve_launch_dir_trust(empty.path(), None);
        assert!(
            DECISIONS.lock().get(&workspace_key(empty.path())).is_none(),
            "provisional no-configs allow must not be cached by resolve_launch_dir_trust"
        );
        assert_eq!(lt, resolve_and_record(empty.path(), None, false));
        assert!(lt, "no-configs launch dir must be allowed");

        // (b) Configs present + untrusted (empty store, headless) => false.
        let untrusted = repo_tmp();
        std::fs::create_dir_all(untrusted.path().join(".echo-agent").join("hooks")).unwrap();
        let lt = resolve_launch_dir_trust(untrusted.path(), None);
        assert_eq!(lt, resolve_and_record(untrusted.path(), None, false));
        assert!(!lt, "untrusted configs launch dir must be denied");

        // (c) Configs present + store-trusted => true.
        let trusted = repo_tmp();
        std::fs::create_dir_all(trusted.path().join(".echo-agent").join("hooks")).unwrap();
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(trusted.path())).unwrap();
        let lt = resolve_launch_dir_trust(trusted.path(), None);
        assert_eq!(lt, resolve_and_record(trusted.path(), None, false));
        assert!(lt, "store-trusted launch dir must be allowed");
    }

    #[test]
    #[serial_test::serial]
    fn local_build_is_inert_launch_trust_auto_trusts() {
        // On a local/dev build the whole folder-trust system is inert: an
        // untrusted repo that HAS repo-local configs (here an `.envrc`) with an
        // EMPTY store still resolves trusted, `resolve_launch_dir_trust` returns
        // true, and the `.envrc` loads without any grant. Assert the local branch
        // ONLY when compiled unstamped (mirrors the workspace
        // `is_local_build_honors_test_version_override`), with ECHO_AGENT_TEST_VERSION
        // unset so `is_local_build()` is genuinely true. ECHO_AGENT_HOME-isolated so the
        // real store is never touched.
        let _sim = EnvGuard::unset(echo_agent_version::TEST_VERSION_ENV);
        if option_env!("ECHO_AGENT_VERSION").is_some() {
            return; // a release-stamped test binary is not a local build
        }
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let tmp = repo_tmp();
        std::fs::write(tmp.path().join(".envrc"), "export LOCAL_BUILD_ENVRC=1\n").unwrap();

        // The `.envrc` makes this a gating-eligible repo: on a release build it
        // would resolve untrusted with an empty store. On a local build it does not.
        assert!(
            repo_configs_present(tmp.path()),
            "the `.envrc` must make this a gating-eligible repo"
        );
        assert!(
            project_scope_allowed(tmp.path()),
            "local build: inert gate must auto-trust an untrusted repo that has configs"
        );
        assert!(
            resolve_launch_dir_trust(tmp.path(), None),
            "local build: launch-dir verdict must be trusted"
        );

        // The gated `.envrc` load (the call-site contract) runs because the gate
        // is inert/trusted, so the var is present with no store grant.
        let env = echo_agent_workspace::envrc::load_envrc_or_empty(tmp.path());
        assert_eq!(
            env.get("LOCAL_BUILD_ENVRC"),
            Some(&"1".to_string()),
            "local build: `.envrc` must load without any store grant"
        );
    }

    #[test]
    #[serial_test::serial]
    fn prompt_warranted_true_for_untrusted_repo_with_configs() {
        // Feature on (via remote), untrusted (empty store), repo configs present
        // => the GUI prompt is warranted. ECHO_AGENT_HOME-isolated so the store starts
        // empty; `#[serial]` because ECHO_AGENT_HOME is process-global.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let tmp = repo_tmp();
        std::fs::write(tmp.path().join(".mcp.json"), "{}").unwrap();
        // Simulate a release-stamped build so the inert local-build gate is off
        // and the remote `folder_trust_enabled` flag actually engages.
        // ECHO_AGENT_FOLDER_TRUST unset: env outranks the remote flag, so an ambient
        // opt-out would otherwise false-fail the Prompt assertion.
        let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0.0-sim");
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let remote = RemoteSettings {
            folder_trust_enabled: Some(true),
            ..Default::default()
        };
        assert!(prompt_warranted(tmp.path(), Some(&remote)));
    }

    #[test]
    #[serial_test::serial]
    fn prompt_warranted_false_when_feature_disabled() {
        // The remote kill-switch (folder_trust_enabled = Some(false)) disables the
        // feature even on a release-stamped build, so no prompt is warranted even
        // with repo configs present. Simulate a release build so the inert
        // local-build path is not what's under test; ECHO_AGENT_HOME-isolated and
        // ECHO_AGENT_FOLDER_TRUST unset so the kill-switch is the only signal.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
        let _sim = simulate_release_build();
        let tmp = repo_tmp();
        std::fs::write(tmp.path().join(".mcp.json"), "{}").unwrap();
        let remote = RemoteSettings {
            folder_trust_enabled: Some(false),
            ..Default::default()
        };
        assert!(!prompt_warranted(tmp.path(), Some(&remote)));
    }

    #[test]
    #[serial_test::serial]
    fn prompt_warranted_false_when_store_trusted() {
        // A folder the user already trusted resolves Trusted, not Prompt.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let tmp = repo_tmp();
        std::fs::write(tmp.path().join(".mcp.json"), "{}").unwrap();
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(tmp.path())).unwrap();
        let remote = RemoteSettings {
            folder_trust_enabled: Some(true),
            ..Default::default()
        };
        assert!(!prompt_warranted(tmp.path(), Some(&remote)));
    }

    #[test]
    #[serial_test::serial]
    fn prompt_warranted_false_without_repo_configs() {
        // Nothing repo-local to gate => Trusted, not Prompt.
        let home = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
        let tmp = repo_tmp();
        let remote = RemoteSettings {
            folder_trust_enabled: Some(true),
            ..Default::default()
        };
        assert!(!prompt_warranted(tmp.path(), Some(&remote)));
    }

    #[test]
    fn detected_config_kinds_summarizes_present_markers() {
        let tmp = repo_tmp();
        std::fs::write(tmp.path().join(".mcp.json"), "{}").unwrap();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("hooks")).unwrap();
        std::fs::write(tmp.path().join(".echo-agent").join("lsp.json"), "{}").unwrap();
        std::fs::write(tmp.path().join(".envrc"), "export X=1\n").unwrap();
        let kinds = detected_config_kinds(tmp.path());
        assert!(kinds.contains(&"mcp".to_string()));
        assert!(kinds.contains(&"hooks".to_string()));
        assert!(kinds.contains(&"envrc".to_string()));
        // `lsp` is reported (a real gate reason), NOT filtered out — so an
        // lsp-only repo never prompts with an empty reason list.
        assert!(kinds.contains(&"lsp".to_string()));
    }

    #[test]
    fn detected_config_kinds_reports_lsp_only_repo() {
        // Regression for the "empty configKinds" bug: a repo gated SOLELY by
        // `.echo-agent/lsp.json` must still produce a non-empty reason list.
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent")).unwrap();
        std::fs::write(tmp.path().join(".echo-agent").join("lsp.json"), "{}").unwrap();
        let kinds = detected_config_kinds(tmp.path());
        assert_eq!(kinds, vec!["lsp".to_string()]);
    }
}
