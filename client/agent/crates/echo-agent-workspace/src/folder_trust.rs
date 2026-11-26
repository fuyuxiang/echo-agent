//! Folder-trust DECISION side ("do you trust this folder?").
//!
//! This is the client/workspace half of the folder-trust gate: it scans a
//! workspace for repo-local code-exec configs, resolves the pure trust
//! [`decide`] precedence, prompts (MVP stderr), and reads/writes the durable
//! [`crate::trust::TrustStore`] (`~/.echo-agent/trusted_folders.toml`). The
//! consume/gating half (the `DECISIONS` cache, `resolve_and_record`,
//! `project_scope_allowed`, the loader filters) lives in `echo-agent-runtime`.
//!
//! ## Precedence (canonical — see [`decide`])
//! 1. Feature flag OFF  → trusted (no gating; preserves prior behavior).
//! 2. Store (self/ancestor recorded trusted) → trusted. An explicit `--trust`
//!    grant is persisted to the store up front (see [`grant_folder_trust`]), so
//!    it is honored here.
//! 3. Key unrecordable (an over-broad root — the user's own `$HOME` / filesystem
//!    root / non-absolute — that the store refuses to persist) → trusted: it
//!    can't be durably gated, so gating would re-prompt forever on a key that can
//!    never persist. See [`crate::trust::is_unsafe_trust_root`].
//! 4. No repo-local code-exec configs present → trusted (nothing to gate).
//! 5. Interactive TTY   → prompt the user (y/N).
//! 6. Otherwise (headless) → untrusted.
//!
//! (How the consume side caches this verdict — e.g. that the rule-4 allow is
//! provisional and re-checked rather than cached — is a `echo-agent-runtime`
//! concern, documented there.)

use std::io::IsTerminal;
use std::path::Path;

use echo_agent_config_types::{BoolFlag, RemoteSettings};
use toml::Value as TomlValue;

use crate::trust::{TrustStore, workspace_key};

/// The pure trust outcome for a set of inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustOutcome {
    /// Repo-local servers allowed.
    Trusted,
    /// Repo-local servers blocked.
    Untrusted,
    /// Interactive: ask the user.
    Prompt,
}

/// Inputs to the pure [`decide`] precedence function.
#[derive(Debug, Clone, Copy)]
pub struct DecideInputs {
    pub store_trusted: bool,
    pub repo_configs_present: bool,
    pub is_interactive: bool,
    /// False when the workspace key is an over-broad root the store refuses to
    /// record — home / filesystem root / non-absolute; see
    /// [`crate::trust::is_unsafe_trust_root`].
    pub key_recordable: bool,
}

/// Pure trust-decision precedence. No I/O; unit-tested directly.
///
/// See the module docs for the ordered precedence.
pub fn decide(feature_enabled: bool, i: &DecideInputs) -> TrustOutcome {
    if !feature_enabled {
        return TrustOutcome::Trusted;
    }
    if i.store_trusted {
        return TrustOutcome::Trusted;
    }
    // An over-broad root the store can't record (the user's own $HOME / fs-root,
    // never a fetched repo) can't be durably gated — trust it instead of
    // prompting on a key that can never persist (mirrors the feature-off default).
    if !i.key_recordable {
        return TrustOutcome::Trusted;
    }
    if !i.repo_configs_present {
        return TrustOutcome::Trusted;
    }
    if i.is_interactive {
        return TrustOutcome::Prompt;
    }
    TrustOutcome::Untrusted
}

/// Gather the [`DecideInputs`] for `cwd` (store trust + repo configs +
/// interactivity), keyed by `key`. Single-sourced gather behind the shell's
/// `compute` and launch-dir resolve so the store read and repo-config scan
/// cannot drift across callers.
pub fn decide_inputs(cwd: &Path, key: &Path) -> DecideInputs {
    decide_inputs_with_interactive(cwd, key, is_interactive())
}

/// Like [`decide_inputs`] but with caller-supplied interactivity, so the gather
/// (store trust + repo-config scan) stays single-sourced across callers that
/// determine interactivity differently. The pager TUI passes
/// `stdin().is_terminal()` ONLY: it redirects native stderr before resolving
/// trust, so the default [`is_interactive`] (`stdin && stderr`) would be false
/// and the question could never show.
pub fn decide_inputs_with_interactive(
    cwd: &Path,
    key: &Path,
    is_interactive: bool,
) -> DecideInputs {
    DecideInputs {
        store_trusted: TrustStore::load().is_trusted(key),
        // Deliberate second discover: the caller's `key` came from `workspace_key`
        // (its own git2 discover), and `repo_configs_present` → `RepoDirChain::resolve`
        // discovers the same repo again. Collapsing the two would mean threading the
        // resolved root into key derivation (rippling `workspace_key` repo-wide) — out
        // of scope; NOT the redundant discovers this change already removed.
        repo_configs_present: repo_configs_present(cwd),
        is_interactive,
        // An over-broad key (home / fs-root / non-absolute) can never be recorded
        // by the store, so decide() trusts it rather than prompt on a key that
        // can't persist (Case 2: cwd IS $HOME, incl. the default `~/.echo-agent`).
        key_recordable: !crate::trust::is_unsafe_trust_root(key),
    }
}

/// Whether the whole folder-trust system is inert (auto-trusts everything) for
/// this binary — true on a local/dev build (no `ECHO_AGENT_VERSION` release stamp).
///
/// THE single security short-circuit: every explicit trust auto-grant site calls
/// this (greppable via `folder_trust_inert`). When true a self-built echoagent never
/// prompts, never gates repo-local `.envrc`/`.claude`/hooks/plugins/MCP/LSP, and
/// does NO `trusted_folders.toml` I/O. Release-stamped builds are unaffected.
pub fn folder_trust_inert() -> bool {
    is_local_build()
}

/// Whether this binary was built without a release version stamp
/// (`ECHO_AGENT_VERSION` unset at compile time) — i.e. a local/dev build.
///
/// Kept local (not in `echo-agent-version`) on purpose: adding a symbol to that
/// near-universal crate widens the rebuild/test fan-out for unrelated targets.
/// `option_env!` resolves the same in any crate, so the
/// location is behavior-neutral. Cross-crate callers use [`folder_trust_inert`].
fn is_local_build() -> bool {
    // Runtime escape hatch: a pinned ECHO_AGENT_TEST_VERSION simulates a release build,
    // so tests/CI (which run unstamped, i.e. local-looking) can exercise the gate.
    if std::env::var(echo_agent_version::TEST_VERSION_ENV).is_ok() {
        return false;
    }
    option_env!("ECHO_AGENT_VERSION").is_none()
}

/// Resolve whether the folder-trust gate is enabled.
///
/// On a local/dev build (no `ECHO_AGENT_VERSION` release stamp) the feature is OFF
/// regardless of env/config/remote — a self-built echoagent auto-trusts (never
/// prompts, never gates repo-local MCP/LSP). Folder-trust applies only to
/// shipped, release-stamped binaries.
///
/// On a release-stamped build, normal precedence (via `BoolFlag`):
/// env `ECHO_AGENT_FOLDER_TRUST` > `[folder_trust] enabled` (user) > managed >
/// remote `folder_trust_enabled` > default **true** (on by default; the remote
/// `folder_trust_enabled` kill-switch or a `[folder_trust] enabled = false`
/// opt-out turns it back off).
pub fn feature_enabled(remote: Option<&RemoteSettings>) -> bool {
    feature_enabled_for_build(remote, is_local_build())
}

/// `feature_enabled` with the local-build flag fed in so both arms are unit-testable.
fn feature_enabled_for_build(remote: Option<&RemoteSettings>, is_local_build: bool) -> bool {
    // Local/dev builds never gate (auto-trust): folder-trust applies only to
    // shipped, release-stamped binaries. Even an explicit ECHO_AGENT_FOLDER_TRUST/config
    // opt-in is ignored here so a self-built echoagent never prompts.
    if is_local_build {
        return false;
    }
    fn from_toml(v: Option<&TomlValue>) -> Option<bool> {
        v?.get("folder_trust")?.get("enabled")?.as_bool()
    }
    let user = echo_agent_config::load_from_disk().ok();
    let managed = echo_agent_config::load_managed_config().ok();
    BoolFlag::env("ECHO_AGENT_FOLDER_TRUST")
        .config(from_toml(user.as_ref()))
        .managed(from_toml(managed.as_ref()))
        .feature_flag(remote.and_then(|r| r.folder_trust_enabled))
        .default(true)
        .resolve()
        .value
}

/// Persist an explicit `--trust` grant for `cwd`'s workspace so repo-local
/// servers are honored on the next resolve. Done client-side because trust is
/// durable: even when the agent runs in a separate leader process it reads the
/// same `~/.echo-agent/trusted_folders.toml`. Best-effort; a write failure is logged,
/// not fatal.
pub fn grant_folder_trust(cwd: &Path) {
    // Local/dev builds never gate, so there is nothing to grant: `--trust` is a
    // no-op and the store is left untouched (the whole feature is inert).
    if folder_trust_inert() {
        return;
    }
    persist_trust(&mut TrustStore::load(), &workspace_key(cwd));
}

/// Store-only half of revoking trust for `cwd`'s workspace: persist an explicit
/// `set_untrusted` ONLY when the folder was actually trusted, and report whether
/// it had been trusted. The in-process `DECISIONS` cache downgrade is the shell
/// wrapper's job (the cache lives there).
///
/// Writing a store deny for a never-trusted folder would record a most-specific
/// child DENY that poisons a future ancestor `set_trusted` cascade — so that
/// write stays gated. Symmetric with [`grant_folder_trust`].
pub fn revoke_folder_trust_store(cwd: &Path) -> bool {
    // Local/dev builds never wrote the store, so there is nothing to revoke.
    if folder_trust_inert() {
        return false;
    }
    let key = workspace_key(cwd);
    let mut store = TrustStore::load();
    let was_trusted = store.is_trusted(&key);
    // Persist an explicit deny ONLY for an actually-trusted folder: writing one
    // for a never-trusted folder would record a most-specific child DENY that
    // overrides a future ancestor `set_trusted` grant (cascade poisoning).
    if was_trusted && let Err(e) = store.set_untrusted(&key) {
        tracing::warn!(
            path = %key.display(),
            error = %e,
            "folder trust: failed to persist untrust decision"
        );
    }
    was_trusted
}

pub fn persist_trust(store: &mut TrustStore, key: &Path) {
    if let Err(e) = store.set_trusted(key) {
        tracing::warn!(
            path = %key.display(),
            error = %e,
            "folder trust: failed to persist trust decision"
        );
    }
}

/// Whether any repo-local trust-sensitive config is present for `cwd`. When none
/// are present there is nothing to gate, so we skip the prompt entirely.
///
/// Thin wrapper over [`collect_repo_config_kinds`] with `first_only = true`, so
/// the gate and the display-only [`repo_config_kinds`] enumerate the EXACT same
/// markers (they cannot drift) while this hot path still short-circuits on the
/// first hit.
pub fn repo_configs_present(cwd: &Path) -> bool {
    !collect_repo_config_kinds(cwd, true).is_empty()
}

/// Display-only: which repo-local trust-sensitive config KINDS are present for
/// `cwd` (`mcp`, `plugins`, `permission`, `lsp`, `envrc`, `claude`, `hooks`,
/// `agents`, `roles`, `personas`, `workflows`), deduped in cheap→expensive
/// marker order. Single source with [`repo_configs_present`] (which is
/// `!repo_config_kinds(cwd).is_empty()`), so a folder that the gate fired on
/// always has a non-empty, accurate kind list — no `[plugins].paths` /
/// `[permission]` / `.claude` / `.echo-agent/agents` / subdir-launch gaps. NOT itself
/// the trust gate.
pub fn repo_config_kinds(cwd: &Path) -> Vec<&'static str> {
    collect_repo_config_kinds(cwd, false)
}

/// Whether a project `.echo-agent/config.toml` `[permission]` value would contribute
/// rules to the permission resolver. Mirrors the compact/verbose shapes that
/// `permission::resolution` loads: non-empty `allow`/`deny`/`ask` string arrays,
/// or a non-empty verbose `rules` array. Empty arrays / empty tables do not gate
/// (same as empty `[mcp_servers]` / empty `[plugins].paths`).
fn config_toml_permission_contributes(permission_value: &TomlValue) -> bool {
    let Some(table) = permission_value.as_table() else {
        // Non-table `[permission]` fails config load elsewhere; treat as a
        // marker so a malicious non-table still trips the gate rather than
        // resolving trusted.
        return true;
    };
    for key in ["deny", "allow", "ask"] {
        if table
            .get(key)
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
        {
            return true;
        }
    }
    table
        .get("rules")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
}

fn path_present_or_uncertain(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn directory_present_or_uncertain(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_dir(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Shared scanner behind [`repo_configs_present`] and [`repo_config_kinds`]. With
/// `first_only` it returns immediately after the first marker (the gate's
/// historical short-circuit); otherwise it collects every distinct kind.
fn collect_repo_config_kinds(cwd: &Path, first_only: bool) -> Vec<&'static str> {
    // Resolve the git root + cwd→root dir chain ONCE and reuse it across the
    // git2-based marker checks below: this gate does 1 git2 discover + 1 git2
    // walk (+ the settings-compat path's own cheap `.git`-existence walk, intentionally separate —
    // see its check). Each walker used to run its own git2 discover + walk (~5
    // discovers), and on a non-git dir each discover walks to the filesystem root
    // — wasteful anywhere, and Windows taxes every such syscall 10-100x.
    // Cheap→expensive, short-circuiting on first hit when `first_only`.
    let chain = echo_agent_core::repo::RepoDirChain::resolve(cwd);
    let mut kinds: Vec<&'static str> = Vec::new();
    // Record a distinct kind; when `first_only`, return as soon as one is found
    // (preserves the gate's first-hit short-circuit exactly).
    macro_rules! hit {
        ($k:expr) => {{
            let k: &'static str = $k;
            if !kinds.contains(&k) {
                kinds.push(k);
            }
            if first_only {
                return kinds;
            }
        }};
    }

    // `.mcp.json` anywhere from repo root down to cwd.
    if !crate::project_config::find_mcp_json_files_in(&chain.dirs).is_empty() {
        hit!("mcp");
    }
    // Project `.echo-agent/config.toml` declaring repo-controlled code-exec or
    // permission policy: a non-empty `[mcp_servers]` table, a non-empty
    // `[plugins].paths` array, OR a contributing `[permission]` section.
    // `[plugins].paths` loads as auto-trusted ConfigPath plugins; `[permission]`
    // allow/deny/ask rules auto-approve or block tools — a clone whose ONLY
    // repo-local config is either must still be gated (else it resolves Trusted
    // and the loader runs ungated).
    for path in crate::project_config::find_project_configs_in(&chain.dirs) {
        let Ok(root) = echo_agent_config::load_config_file(&path) else {
            continue;
        };
        let has_mcp_servers = root
            .get("mcp_servers")
            .and_then(|v| v.as_table())
            .is_some_and(|t| !t.is_empty());
        let has_plugin_paths = root
            .get("plugins")
            .and_then(|v| v.get("paths"))
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        let has_permission = root
            .get("permission")
            .is_some_and(config_toml_permission_contributes);
        if has_mcp_servers {
            hit!("mcp");
        }
        if has_plugin_paths {
            hit!("plugins");
        }
        if has_permission {
            hit!("permission");
        }
    }
    // Project `.echo-agent/lsp.json`.
    if cwd.join(".echo-agent").join("lsp.json").is_file() {
        hit!("lsp");
    }
    // Project `.cursor/mcp.json` — vendor MCP loading is default-on and tagged
    // `Project`, so a repo shipping ONLY this file must still be gated (file
    // presence is enough).
    if cwd.join(".cursor").join("mcp.json").is_file() {
        hit!("mcp");
    }
    // Project `.envrc` — auto-sourced in a bash subshell when `direnv` isn't
    // installed (direct code-exec), so an `.envrc`-only clone must still be
    // gated. The loader reads `<cwd>/.envrc` directly (NOT a git-root walk), so
    // probe at cwd to match exactly what gets executed.
    if cwd.join(".envrc").is_file() {
        hit!("envrc");
    }
    // Project `.claude/settings.json` / `settings.local.json`: the hooks surface
    // reads these at the git root, but the ENV/permission loaders walk EVERY dir
    // cwd→repo-root (`collect_project_claude_paths`), so detect along the SAME
    // walk via the shared reader — else a `.claude` `env` in a SUBDIR (injected
    // into every spawned subprocess) loads ungated. Subsumes the git-root probe.
    // Keeps its own `.git`-existence walk (NOT the git2 chain) so detection stays
    // identical to the loader, which bounds on a bare/empty `.git` too.
    if crate::permission::claude_settings::project_claude_settings_present(cwd) {
        hit!("claude");
    }
    // Other project HOOK sources are resolved from the git worktree root only
    // (the chain's `git_root`, the same root hook discovery resolves from via
    // `workspace_key`), NOT cwd, so root-level hooks are gated even when launched
    // from a subdir. A repo-local hook file/dir is repo-controlled code-exec that
    // must be gated — else a hooks-only clone (e.g. `.echo-agent/hooks/evil.json`) would
    // resolve trusted and run ungated. Presence mirrors discovery's "something to
    // gate" check.
    let hook_root = chain.git_root.as_deref().unwrap_or(cwd);
    if path_present_or_uncertain(&hook_root.join(".echo-agent").join("hooks"))
        || hook_root.join(".cursor").join("hooks.json").is_file()
    {
        hit!("hooks");
    }
    // Project PLUGIN dirs: project-scoped plugins are unified under folder-trust
    // too, so a repo-local plugin dir is repo-controlled code-exec (hooks/MCP)
    // that must be gated — else a plugin clone (e.g. `.echo-agent/plugins/evil/`, even
    // one in a subdir launched via `cd sub && echoagent`) would resolve trusted and
    // run ungated. Uses the shared SSOT walk (cwd→git root) so detection matches
    // exactly what `discover_plugins` scans for Project scope (errs secure).
    if !echo_agent_core::plugins::project_plugin_dirs_in(&chain.dirs).is_empty() {
        hit!("plugins");
    }
    // Project AGENT dirs (`.echo-agent/agents` / `.claude/agents`): a project agent
    // definition can carry an inline `hooks:` block (repo-controlled code-exec)
    // AND can SHADOW a built-in subagent by name, so an agents-only clone must
    // still be gated. Uses the shared SSOT walk (cwd→git root) so detection
    // can't drift from agent discovery — same pattern as the plugin line above.
    if !echo_agent_core::discovery::project_agent_dirs_in(&chain.dirs).is_empty() {
        hit!("agents");
    }
    // Presence matches exact-cwd discovery without parsing repository content.
    let echoagent = cwd.join(".echo-agent");
    if directory_present_or_uncertain(&echoagent.join("roles")) {
        hit!("roles");
    }
    if directory_present_or_uncertain(&echoagent.join("personas")) {
        hit!("personas");
    }
    if directory_present_or_uncertain(&hook_root.join(".echo-agent").join("workflows")) {
        hit!("workflows");
    }
    // `~/.claude.json` `projects.<cwd>.mcpServers`.
    if claude_project_mcp_present(cwd) {
        hit!("mcp");
    }
    kinds
}

/// Display names under `~/.claude.json projects.<cwd>.mcpServers`, or `None`
/// when the file/entry is absent or the object is empty. Single reader that both
/// [`claude_project_mcp_present`] (existence) and the shell's
/// `project_scoped_mcp_names` (the names) derive from, so the two never drift.
pub fn claude_project_mcp_names(cwd: &Path) -> Option<Vec<String>> {
    let home = echo_agent_config::discovery_home_dir()?;
    let content = std::fs::read_to_string(home.join(".claude.json")).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    let cwd_key = cwd.to_string_lossy();
    let names: Vec<String> = value
        .get("projects")
        .and_then(|p| p.get(cwd_key.as_ref()))
        .and_then(|proj| proj.get("mcpServers"))
        .and_then(|m| m.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    (!names.is_empty()).then_some(names)
}

fn claude_project_mcp_present(cwd: &Path) -> bool {
    claude_project_mcp_names(cwd).is_some()
}

fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// MVP trust prompt: a plain stderr warning + stdin y/N read.
///
/// Defaults to NO on empty input, EOF, or any non-yes answer. Deliberately
/// minimal (no ACP modal); a richer modal is a future follow-up.
pub fn prompt_for_trust(key: &Path) -> bool {
    use std::io::{BufRead, Write};

    let mut err = std::io::stderr();
    let _ = writeln!(err);
    let _ = writeln!(
        err,
        "This folder contains repo-local config (.mcp.json / .echo-agent/lsp.json / hooks) \
         that can run commands on your machine."
    );
    let _ = writeln!(err, "  Folder: {}", key.display());
    let _ = write!(
        err,
        "Trust the authors of this folder and allow these servers to start? [y/N] "
    );
    let _ = err.flush();

    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> DecideInputs {
        DecideInputs {
            store_trusted: false,
            repo_configs_present: true,
            is_interactive: false,
            // Default: a normal (recordable) key, so the Case-2 rule doesn't fire
            // and every `..inputs()` spread exercises the pre-existing precedence.
            key_recordable: true,
        }
    }

    #[test]
    fn feature_off_is_always_trusted() {
        // Even with everything pointing to untrusted, feature off => trusted.
        assert_eq!(decide(false, &inputs()), TrustOutcome::Trusted);
    }

    #[test]
    fn store_trusted_is_trusted() {
        let i = DecideInputs {
            store_trusted: true,
            ..inputs()
        };
        assert_eq!(decide(true, &i), TrustOutcome::Trusted);
    }

    #[test]
    fn no_repo_configs_is_trusted_without_prompt() {
        let i = DecideInputs {
            repo_configs_present: false,
            is_interactive: true,
            ..inputs()
        };
        // Nothing to gate => trusted, and crucially NOT Prompt.
        assert_eq!(decide(true, &i), TrustOutcome::Trusted);
    }

    #[test]
    fn interactive_with_configs_prompts() {
        let i = DecideInputs {
            is_interactive: true,
            ..inputs()
        };
        assert_eq!(decide(true, &i), TrustOutcome::Prompt);
    }

    #[test]
    fn headless_with_configs_is_untrusted() {
        assert_eq!(decide(true, &inputs()), TrustOutcome::Untrusted);
    }

    #[test]
    fn unrecordable_key_is_trusted_even_with_configs_and_interactive() {
        // Case 2: cwd == $HOME (or fs-root / non-absolute). The store can't record
        // such a key, so gating would re-prompt forever — decide() trusts it,
        // ahead of the repo-configs and interactive rules.
        let i = DecideInputs {
            store_trusted: false,
            repo_configs_present: true,
            is_interactive: true,
            key_recordable: false,
        };
        assert_eq!(decide(true, &i), TrustOutcome::Trusted);
    }

    /// A `git init`'d temp dir so `find_mcp_json_files` / `find_project_configs`
    /// (which discover the enclosing repo and walk to its root) are bounded to
    /// the temp dir instead of any ancestor repo the system temp dir lives in.
    fn repo_tmp() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        tmp
    }

    #[test]
    fn repo_configs_present_false_when_empty() {
        let tmp = repo_tmp();
        assert!(!repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_mcp_json() {
        let tmp = repo_tmp();
        std::fs::write(tmp.path().join(".mcp.json"), "{}").unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_echo_agent_config_mcp_servers() {
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            "[mcp_servers.x]\ncommand=\"y\"\n",
        )
        .unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_echo_agent_lsp_json() {
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(echoagent.join("lsp.json"), "{}").unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_cursor_mcp_json() {
        let tmp = repo_tmp();
        let cursor = tmp.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(cursor.join("mcp.json"), "{}").unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_envrc() {
        // An `.envrc`-only clone is auto-sourced in a bash subshell (direct RCE),
        // so it must resolve untrusted even though it has no MCP/LSP/hook configs.
        let tmp = repo_tmp();
        std::fs::write(tmp.path().join(".envrc"), "export FOO=bar\n").unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_project_agents() {
        // A `.echo-agent/agents`-only clone must be gated: a project agent definition
        // can carry an inline `hooks:` block (code-exec) and can shadow a built-in
        // subagent by name.
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("agents")).unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_claude_agents() {
        // `.claude/agents` is the vendor-compat project agent dir; same gate.
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".claude").join("agents")).unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_project_agents_from_subdir() {
        // Agents live at the git root but the session is launched from a subdir;
        // detection walks cwd→git root exactly like agent discovery, so it must
        // still fire (a cwd-only probe would miss it).
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("agents")).unwrap();
        let subdir = tmp.path().join("crates").join("inner");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(repo_configs_present(&subdir));
    }

    #[test]
    fn repo_configs_present_detects_project_roles() {
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("roles")).unwrap();

        assert!(repo_configs_present(tmp.path()));
        assert!(repo_config_kinds(tmp.path()).contains(&"roles"));
    }

    #[test]
    fn repo_configs_present_detects_project_personas() {
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("personas")).unwrap();

        assert!(repo_configs_present(tmp.path()));
        assert!(repo_config_kinds(tmp.path()).contains(&"personas"));
    }

    #[test]
    fn project_subagent_marker_regular_file_is_absent() {
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(echoagent.join("roles"), "not a directory").unwrap();
        assert!(!repo_configs_present(tmp.path()));
    }

    #[test]
    fn project_subagent_marker_at_repo_root_is_absent_from_subdir() {
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent/roles")).unwrap();
        let subdir = tmp.path().join("nested");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(!repo_configs_present(&subdir));
    }

    #[cfg(unix)]
    #[test]
    fn project_subagent_marker_symlink_to_directory_is_present() {
        let tmp = repo_tmp();
        let target = tmp.path().join("target-roles");
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&echoagent).unwrap();
        std::os::unix::fs::symlink(&target, echoagent.join("roles")).unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_project_subagent_marker_is_absent() {
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::os::unix::fs::symlink("missing", echoagent.join("personas")).unwrap();
        assert!(!repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_project_workflows_from_subdir() {
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("workflows")).unwrap();
        let subdir = tmp.path().join("crates").join("inner");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(repo_configs_present(&subdir));
        assert!(repo_config_kinds(&subdir).contains(&"workflows"));
    }

    #[test]
    fn repo_configs_present_detects_claude_settings_from_subdir() {
        // A `.claude/settings.json` `env` in a SUBDIR (no other repo config),
        // launched from that subdir, must be detected: the env loader walks
        // cwd→repo-root, so detection walks the same path (a git-root-only probe
        // would miss it and leave the env injectable ungated).
        let tmp = repo_tmp();
        let subdir = tmp.path().join("crates").join("inner");
        let claude = subdir.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("settings.json"), r#"{"env":{"X":"1"}}"#).unwrap();
        assert!(repo_configs_present(&subdir));
    }

    #[test]
    fn repo_configs_present_detects_project_hooks() {
        // A hooks-only repo (no MCP/LSP configs) must still be gated, so its
        // project hooks don't run ungated when the folder is untrusted.
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("hooks")).unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_project_hooks_file() {
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(echoagent.join("hooks"), "{}").unwrap();

        assert!(repo_configs_present(tmp.path()));
        assert!(repo_config_kinds(tmp.path()).contains(&"hooks"));
    }

    #[cfg(unix)]
    #[test]
    fn repo_configs_present_detects_dangling_project_hooks_symlink() {
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::os::unix::fs::symlink("missing-hooks", echoagent.join("hooks")).unwrap();

        assert!(repo_configs_present(tmp.path()));
        assert!(repo_config_kinds(tmp.path()).contains(&"hooks"));
    }

    #[test]
    fn repo_configs_present_detects_project_hooks_from_subdir() {
        // Hooks live at the git root but the session is launched from a subdir;
        // the gate must still fire because discovery resolves hooks from the root
        // (the cwd-relative check this regresses would miss it).
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("hooks")).unwrap();
        let subdir = tmp.path().join("crates").join("inner");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(repo_configs_present(&subdir));
    }

    #[test]
    fn repo_configs_present_detects_project_plugins() {
        // A plugin-only repo (no MCP/LSP/hooks configs) must still be gated, so a
        // project plugin's hooks/MCP don't run ungated when the folder is untrusted.
        let tmp = repo_tmp();
        std::fs::create_dir_all(tmp.path().join(".echo-agent").join("plugins").join("x")).unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_project_plugins_in_subdir() {
        // A plugin under a subdir (root otherwise clean), launched from that
        // subdir, must still be gated: detection walks cwd→git root exactly like
        // discover_plugins, so a subdir-only plugin is not a fail-open hole.
        let tmp = repo_tmp();
        let subdir = tmp.path().join("packages").join("foo");
        std::fs::create_dir_all(subdir.join(".echo-agent").join("plugins").join("evil")).unwrap();
        assert!(repo_configs_present(&subdir));
    }

    #[test]
    fn repo_configs_present_false_for_empty_mcp_servers_table() {
        // A project config whose `[mcp_servers]` table is empty has nothing to
        // gate, so it must not trip the gate.
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(echoagent.join("config.toml"), "[mcp_servers]\n").unwrap();
        assert!(!repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_echo_agent_config_plugins_paths() {
        // A repo whose ONLY repo-local config is `[plugins].paths` (no plugin
        // dir, no MCP/LSP/hooks) must still be gated: those paths load as
        // auto-trusted ConfigPath plugins, so an ungated clone is a live RCE.
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            "[plugins]\npaths = [\"./x\"]\n",
        )
        .unwrap();
        assert!(repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_false_for_empty_plugins_paths() {
        // An empty `[plugins].paths` (or a `[plugins]` table without `paths`)
        // contributes no plugin code-exec, so it must not trip the gate.
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(echoagent.join("config.toml"), "[plugins]\npaths = []\n").unwrap();
        assert!(!repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_configs_present_detects_echo_agent_config_permission() {
        // A repo whose ONLY repo-local config is a contributing `[permission]`
        // section (no MCP/plugins/hooks) must still be gated: those allow rules
        // auto-approve tool calls, so an ungated clone loads the attacker's
        // policy. Also covers subdir launch (cwd→git-root walk).
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            "[permission]\nallow = [\"Bash(*)\"]\n",
        )
        .unwrap();
        assert!(repo_configs_present(tmp.path()));
        assert!(
            repo_config_kinds(tmp.path()).contains(&"permission"),
            "permission-only repo must report the permission kind"
        );
        let subdir = tmp.path().join("crates").join("inner");
        std::fs::create_dir_all(&subdir).unwrap();
        assert!(
            repo_configs_present(&subdir),
            "permission-only config at git root must gate subdir launches"
        );
    }

    #[test]
    fn repo_configs_present_false_for_empty_permission() {
        // Empty allow/deny/ask arrays contribute no rules, so they must not
        // trip the gate (mirrors empty `[mcp_servers]` / empty `[plugins].paths`).
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            "[permission]\nallow = []\ndeny = []\n",
        )
        .unwrap();
        assert!(!repo_configs_present(tmp.path()));
    }

    #[test]
    fn repo_config_kinds_matches_gate_and_reports_all_kinds() {
        // SSOT guard: `repo_config_kinds` (full scan) must agree with the gate
        // (`repo_configs_present == !repo_config_kinds(..).is_empty()`) AND report
        // the kinds the single-source refactor added — `plugins` via
        // `[plugins].paths`, `claude` via `.claude/settings.json`, `agents` via
        // `.echo-agent/agents` — even when launched from a SUBDIR (the cwd→git-root walk
        // that `first_only` shares). Guards against silent drift between the two.
        let tmp = repo_tmp();
        let echoagent = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(echoagent.join("agents")).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            "[plugins]\npaths = [\"./x\"]\n",
        )
        .unwrap();
        let claude = tmp.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("settings.json"), r#"{"env":{"X":"1"}}"#).unwrap();
        // Launch from a subdir: the walk must still find the root-level markers.
        let subdir = tmp.path().join("crates").join("inner");
        std::fs::create_dir_all(&subdir).unwrap();

        let kinds = repo_config_kinds(&subdir);
        for expected in ["plugins", "claude", "agents"] {
            assert!(
                kinds.contains(&expected),
                "repo_config_kinds missing {expected:?} (subdir launch); got {kinds:?}"
            );
        }
        // Gate ↔ kinds equivalence: a configured repo and an empty one.
        assert_eq!(
            repo_configs_present(&subdir),
            !repo_config_kinds(&subdir).is_empty(),
            "gate must equal !kinds.is_empty() for a configured repo"
        );
        let empty = repo_tmp();
        assert!(!repo_configs_present(empty.path()));
        assert!(repo_config_kinds(empty.path()).is_empty());
        assert_eq!(
            repo_configs_present(empty.path()),
            !repo_config_kinds(empty.path()).is_empty(),
            "gate must equal !kinds.is_empty() for an empty repo"
        );
    }

    // ECHO_AGENT_HOME-isolation idiom mirrored from this crate's `permission::claude_compat`
    // tests (the workspace crate has no `serial_test`/`echo-agent-test-support`
    // dev-dep): nextest runs each test in its own process; `ENV_LOCK` serializes
    // the rare in-process `cargo test` thread, and `EnvVarGuard` restores the prior
    // value on drop so a panic can't leak state. The lock is crate-shared so it
    // also serializes against the other env-mutating test modules (e.g. `trust`,
    // `worktree`) under single-process `cargo test --lib`.
    use crate::ENV_TEST_LOCK as ENV_LOCK;

    // The crate-shared generic env-var guard (one definition in `lib.rs`),
    // aliased here so the existing `EnvVarGuard::set/unset` call sites are unchanged.
    use crate::TestEnvGuard as EnvVarGuard;

    /// Simulate a release-stamped build so store I/O runs (a local/dev build makes
    /// grant/revoke no-ops). Hold the returned guard for the test body.
    fn simulate_release_build() -> EnvVarGuard {
        EnvVarGuard::set(echo_agent_version::TEST_VERSION_ENV, Path::new("0.0.0-sim"))
    }

    #[test]
    fn local_build_ignores_remote_rollout() {
        // A local/dev build never gates (auto-trust): even a remote rollout enable
        // is ignored, so the feature stays off and resolves Trusted with repo
        // configs present + interactive. (Env/config isolated to unset so the
        // remote flag is unambiguously the only enable being dropped here.)
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvVarGuard::unset("ECHO_AGENT_FOLDER_TRUST");

        let remote = RemoteSettings {
            folder_trust_enabled: Some(true),
            ..Default::default()
        };
        let feature = feature_enabled_for_build(Some(&remote), true);
        assert!(!feature);
        let i = DecideInputs {
            is_interactive: true,
            ..inputs()
        };
        assert_eq!(decide(feature, &i), TrustOutcome::Trusted);
    }

    #[test]
    fn release_build_keeps_gate_when_enabled() {
        // A release-stamped build (is_local_build=false) honors the remote enable,
        // keeping today's gate. Isolate config so neither on-disk user/managed
        // config nor an ambient env flag can override it: empty ECHO_AGENT_HOME (no
        // config.toml/managed_config.toml) + ECHO_AGENT_FOLDER_TRUST unset. nextest's
        // process-per-test makes echo_agent_home()'s OnceLock pick up the temp dir.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvVarGuard::unset("ECHO_AGENT_FOLDER_TRUST");

        let remote = RemoteSettings {
            folder_trust_enabled: Some(true),
            ..Default::default()
        };
        let feature = feature_enabled_for_build(Some(&remote), false);
        assert!(feature);
        let i = DecideInputs {
            is_interactive: true,
            ..inputs()
        };
        assert_eq!(decide(feature, &i), TrustOutcome::Prompt);
    }

    #[test]
    fn local_build_ignores_explicit_env_optin() {
        // Auto-trust is absolute on a local build: even an explicit
        // ECHO_AGENT_FOLDER_TRUST=1 does NOT enable the feature (so a self-built echoagent
        // never prompts). ECHO_AGENT_HOME isolated so on-disk config can't influence it.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvVarGuard::set("ECHO_AGENT_FOLDER_TRUST", Path::new("1"));

        assert!(!feature_enabled_for_build(None, true));
    }

    #[test]
    fn release_build_defaults_on() {
        // A release-stamped build with no env/config/managed/remote signal defaults
        // the feature ON. Empty ECHO_AGENT_HOME (no config.toml/managed config) +
        // ECHO_AGENT_FOLDER_TRUST unset so only the default applies.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("ECHO_AGENT_HOME", home.path());
        let _flag = EnvVarGuard::unset("ECHO_AGENT_FOLDER_TRUST");

        assert!(feature_enabled_for_build(None, false));
    }

    #[test]
    fn is_local_build_honors_test_version_override() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A pinned ECHO_AGENT_TEST_VERSION simulates a release build => not a local build.
        {
            let _sim =
                EnvVarGuard::set(echo_agent_version::TEST_VERSION_ENV, Path::new("0.0.0-sim"));
            assert!(!is_local_build());
        }
        // With it unset, an unstamped build (no ECHO_AGENT_VERSION) is a local build.
        // Guard to the unstamped case so a release-stamped test binary (CI release)
        // doesn't spuriously fail this arm.
        let _unset = EnvVarGuard::unset(echo_agent_version::TEST_VERSION_ENV);
        if option_env!("ECHO_AGENT_VERSION").is_none() {
            assert!(is_local_build());
        }
    }

    #[test]
    fn store_io_is_noop_on_local_build() {
        // On a local/dev build the whole feature is inert. Both halves pin a guard
        // via a UNIQUE per-repo key (never store-file existence) so they hold under
        // single-process `cargo test` too. Assert ONLY when compiled unstamped
        // (mirrors `is_local_build_honors_test_version_override`); ECHO_AGENT_HOME-isolated
        // and ENV_LOCK-serialized so toggling ECHO_AGENT_TEST_VERSION is race-safe.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("ECHO_AGENT_HOME", home.path());
        let _unset = EnvVarGuard::unset(echo_agent_version::TEST_VERSION_ENV);
        if option_env!("ECHO_AGENT_VERSION").is_some() {
            return; // a release-stamped test binary is not a local build
        }
        let tmp = repo_tmp();
        let key = workspace_key(tmp.path());

        // grant is a no-op: a local-build grant never trusts the fresh key.
        grant_folder_trust(tmp.path());
        assert!(
            !TrustStore::load().is_trusted(&key),
            "local build: grant_folder_trust must not trust the folder"
        );

        // Seed a genuinely-trusted folder under a simulated release build (so the
        // store actually records the grant); the guard drops at block end => local.
        {
            let _sim = simulate_release_build();
            let mut store = TrustStore::load();
            store.set_trusted(&key).unwrap();
            assert!(
                TrustStore::load().is_trusted(&key),
                "release build: seeding must record the trust grant"
            );
        }

        // revoke is a no-op: a local-build revoke returns false AND leaves the grant
        // intact (without the guard it would `set_untrusted` and return true).
        assert!(
            !revoke_folder_trust_store(tmp.path()),
            "local build: revoke_folder_trust_store must return false"
        );
        assert!(
            TrustStore::load().is_trusted(&key),
            "local build: revoke_folder_trust_store must not untrust the folder"
        );
    }

    #[test]
    fn revoke_folder_trust_store_persists_untrust_for_trusted_folder() {
        // The store half of revoke, tested directly (not just via the shell
        // wrapper): a previously-trusted folder reports was_trusted=true AND gets
        // an explicit `set_untrusted` persisted, so it is untrusted on reload.
        // ECHO_AGENT_HOME-isolated so the seed/deny hit a temp store, not the real file.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _env = EnvVarGuard::set("ECHO_AGENT_HOME", home.path());
        let _sim = simulate_release_build();
        let tmp = repo_tmp();
        let key = workspace_key(tmp.path());

        let mut store = TrustStore::load();
        store.set_trusted(&key).unwrap();
        assert!(TrustStore::load().is_trusted(&key));

        assert!(
            revoke_folder_trust_store(tmp.path()),
            "a trusted folder must report was_trusted=true"
        );
        assert!(
            !TrustStore::load().is_trusted(&key),
            "store-only revoke must persist set_untrusted for a trusted folder"
        );
    }

    #[test]
    fn revoke_folder_trust_store_writes_no_deny_for_never_trusted_folder() {
        // The cascade-poisoning guard: revoking a NEVER-trusted child returns
        // false and writes NO explicit child deny, so a later ancestor grant still
        // cascades to the child (a spurious child `set_untrusted` would win
        // most-specific and break the cascade). This store half does NOT touch the
        // `DECISIONS` cache — that downgrade is the shell wrapper's job.
        // ECHO_AGENT_HOME-isolated so the grant writes to a temp store.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _env = EnvVarGuard::set("ECHO_AGENT_HOME", home.path());
        let _sim = simulate_release_build();
        // Distinct git roots so `workspace_key` keeps parent/child as separate
        // keys (the child's own `.git` stops discovery at the child).
        let parent = repo_tmp();
        let child = parent.path().join("child");
        std::fs::create_dir_all(&child).unwrap();
        git2::Repository::init(&child).unwrap();

        assert!(
            !revoke_folder_trust_store(&child),
            "revoking a never-trusted folder must return false"
        );

        // No child deny was written, so an ancestor grant still cascades down.
        let mut store = TrustStore::load();
        store.set_trusted(&workspace_key(parent.path())).unwrap();
        assert!(
            TrustStore::load().is_trusted(&workspace_key(&child)),
            "ancestor grant must cascade to a child revoked-while-untrusted (no poisoning deny)"
        );
    }

    #[test]
    fn decide_inputs_flags_home_key_unrecordable() {
        // Case-2 wiring: with cwd == $HOME (git-init'd so workspace_key discovers
        // it as the home git root), the gather flags key_recordable=false and
        // decide() trusts it despite configs + interactive. $HOME is overridden so
        // echo_agent_config::discovery_home_dir()/workspace_key see the tempdir as home.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", home.path());
        git2::Repository::init(home.path()).unwrap();

        let home_key = crate::trust::workspace_key(home.path());
        let home_inputs = decide_inputs_with_interactive(home.path(), &home_key, true);
        assert!(
            !home_inputs.key_recordable,
            "cwd == $HOME must gather key_recordable=false"
        );
        assert_eq!(
            decide(true, &home_inputs),
            TrustOutcome::Trusted,
            "an unrecordable home key resolves Trusted (no prompt, no gate)"
        );

        // A non-home repo subdir key is recordable — the Case-2 rule can't
        // over-trigger for a real fetched repo.
        let repo = repo_tmp();
        let subdir = repo.path().join("pkg");
        std::fs::create_dir_all(&subdir).unwrap();
        let repo_key = crate::trust::workspace_key(&subdir);
        let repo_inputs = decide_inputs_with_interactive(&subdir, &repo_key, true);
        assert!(
            repo_inputs.key_recordable,
            "a non-home repo key must gather key_recordable=true"
        );
    }
}
