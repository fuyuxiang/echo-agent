//! Kernel-enforced deny paths for sandbox profiles.
//!
//! macOS: Seatbelt platform rules via [`nono::CapabilitySet::add_platform_rule`].
//! Linux: Landlock cannot deny a subpath of an allowed tree; read-deny is
//! enforced via bwrap bind-over (see [`crate::bwrap_reexec_command`]).

#[cfg(all(feature = "enforce", unix))]
use nono::CapabilitySet;
#[cfg(all(feature = "enforce", unix))]
use std::path::{Path, PathBuf};

// Glob deny entries (detection, macOS regex translation, Linux launch-time
// expansion) live in a submodule; re-exported so call sites use `deny::…`.
#[cfg(all(feature = "enforce", unix))]
mod glob;

/// Whether a raw config entry is a glob pattern rather than an exact path. True
/// iff it contains a gitignore-style metacharacter (`*`, `?`, `[`). The single
/// classifier for both `deny` entries and `read_only`/`read_write` allow paths.
pub(crate) fn is_glob(entry: &str) -> bool {
    entry.contains(['*', '?', '['])
}
#[cfg(all(feature = "enforce", target_os = "linux"))]
pub(crate) use glob::{DENY_GLOB_CAPS, expand_deny_globs};
#[cfg(all(feature = "enforce", unix))]
pub(crate) use glob::{apply_deny_globs_to_capability_set, partition_deny_entries};

/// Escape a path for use inside a Seatbelt `(literal "...")` / `(subpath "...")`
/// filter (used for both forms, hence the generic name).
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn escape_seatbelt_path(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    // Reject all control chars (matching nono's escape_path); silently passing
    // one through would target a different path than intended.
    if s.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// All literal paths a deny rule must cover on macOS: the as-given path, its
/// canonical form, and the `/private` firmlink alias of each (e.g. `/tmp/x` <->
/// `/private/tmp/x`) so a deny cannot be bypassed via an alias.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn macos_deny_aliases(path: &Path, canonical: &Path) -> Vec<PathBuf> {
    let mut forms: Vec<PathBuf> = vec![path.to_path_buf()];
    if canonical != path {
        forms.push(canonical.to_path_buf());
    }
    for form in forms.clone() {
        if let Some(alias) = toggle_private_prefix(&form)
            && !forms.contains(&alias)
        {
            forms.push(alias);
        }
    }
    forms
}

/// Toggle the macOS `/private` firmlink prefix for `/tmp`, `/var`, `/etc`
/// (e.g. `/private/tmp/x` <-> `/tmp/x`). Returns `None` for unaffected paths.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn toggle_private_prefix(path: &Path) -> Option<PathBuf> {
    let s = path.to_str()?;
    for dir in ["tmp", "var", "etc"] {
        if let Some(rest) = s.strip_prefix(&format!("/private/{dir}"))
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return Some(PathBuf::from(format!("/{dir}{rest}")));
        }
        if let Some(rest) = s.strip_prefix(&format!("/{dir}"))
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return Some(PathBuf::from(format!("/private/{dir}{rest}")));
        }
    }
    None
}

/// Specific Seatbelt write sub-actions denied for a denied path.
///
/// `(deny file-write* ...)` alone does NOT win: nono emits platform rules
/// between the read-allows and the write-allows, so the broad workspace
/// `(allow file-write* (subpath <ws>))` is emitted AFTER our deny and wins by
/// last-match — leaving an in-workspace denied path writable (so `mv x y && cat y`
/// could relocate and read it). Empirically, denying each concrete write
/// sub-action (every one more specific than the `file-write*` grant) makes the
/// deny win regardless of emission order, fully blocking overwrite AND relocation
/// (rename/unlink). This is observed per-operation rule-list behavior, not a
/// guaranteed action-specificity rule — the macOS e2e is the contract.
#[cfg(all(feature = "enforce", target_os = "macos"))]
const SEATBELT_WRITE_DENY_ACTIONS: &[&str] = &[
    "file-write-data",
    "file-write-create",
    "file-write-unlink",
    "file-write-mode",
    "file-write-owner",
    "file-write-flags",
    "file-write-times",
    "file-write-setugid",
];

/// Emit the full read+write deny rule set for a single Seatbelt `filter`
/// (`(literal ...)` or `(subpath ...)`). See [`SEATBELT_WRITE_DENY_ACTIONS`] for
/// why the specific write sub-actions are required in addition to `file-write*`.
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn emit_seatbelt_deny(caps: &mut CapabilitySet, filter: &str) -> anyhow::Result<()> {
    // Read-deny wins via last-match (platform rules are emitted after read-allows).
    caps.add_platform_rule(format!("(deny file-read* {filter})"))?;
    // Catch-all write-deny (wins for out-of-workspace paths with no competing
    // write grant, e.g. ~/.ssh) ...
    caps.add_platform_rule(format!("(deny file-write* {filter})"))?;
    // ... plus action-specific write denies that also win inside the workspace.
    for action in SEATBELT_WRITE_DENY_ACTIONS {
        caps.add_platform_rule(format!("(deny {action} {filter})"))?;
    }
    Ok(())
}

/// Emit write-only Seatbelt deny rules (hook sources stay readable).
#[cfg(all(feature = "enforce", target_os = "macos"))]
fn emit_seatbelt_write_deny(caps: &mut CapabilitySet, filter: &str) -> anyhow::Result<()> {
    caps.add_platform_rule(format!("(deny file-write* {filter})"))?;
    for action in SEATBELT_WRITE_DENY_ACTIONS {
        caps.add_platform_rule(format!("(deny {action} {filter})"))?;
    }
    Ok(())
}

// Unlink blocks rename of the node; create blocks replacement. Specific
// sub-actions (not bare file-write*) win against later allow-write* grants.
#[cfg(all(feature = "enforce", target_os = "macos"))]
const SEATBELT_ANCESTOR_NODE_DENY_ACTIONS: &[&str] = &["file-write-unlink", "file-write-create"];

#[cfg(all(feature = "enforce", target_os = "macos"))]
fn emit_seatbelt_ancestor_node_deny(caps: &mut CapabilitySet, filter: &str) -> anyhow::Result<()> {
    for action in SEATBELT_ANCESTOR_NODE_DENY_ACTIONS {
        caps.add_platform_rule(format!("(deny {action} {filter})"))?;
    }
    Ok(())
}

/// Leaf parent up to deepest containing writable root; outside all roots → empty.
#[cfg(all(feature = "enforce", target_os = "macos"))]
pub(crate) fn ancestors_within_writable_roots(
    path: &Path,
    writable_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let root = writable_roots
        .iter()
        .filter(|r| path == r.as_path() || path.starts_with(r))
        .max_by_key(|r| r.components().count());
    let Some(root) = root else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for anc in echo_agent_config::existing_ancestor_chain(path) {
        if anc == *root || anc.starts_with(root) {
            out.push(anc);
        }
    }
    if path != root.as_path() && root.exists() && !out.iter().any(|p| p == root) {
        out.push(root.clone());
    }
    out
}

/// Write-only deny for hook sources. Linux is a no-op (bwrap).
#[cfg(all(feature = "enforce", unix))]
pub(crate) fn apply_write_deny_paths_to_capability_set(
    caps: &mut CapabilitySet,
    entries: &[(PathBuf, bool)],
    writable_roots: &[PathBuf],
) -> anyhow::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let mut rule_paths = Vec::new();
        let mut ancestor_seen = std::collections::HashSet::new();
        for (path, is_dir) in entries {
            let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.clone());
            let use_subpath = *is_dir || deny_path_is_dir(&canonical);
            for form in macos_deny_aliases(path, &canonical) {
                let Some(escaped) = escape_seatbelt_path(&form) else {
                    anyhow::bail!("cannot escape write-deny path {form:?} for Seatbelt");
                };
                if use_subpath {
                    emit_seatbelt_write_deny(caps, &format!("(literal \"{escaped}\")"))?;
                    emit_seatbelt_write_deny(caps, &format!("(subpath \"{escaped}\")"))?;
                } else {
                    emit_seatbelt_write_deny(caps, &format!("(literal \"{escaped}\")"))?;
                }
                rule_paths.push(form);
            }
            for anc in ancestors_within_writable_roots(path, writable_roots) {
                if !ancestor_seen.insert(anc.clone()) {
                    continue;
                }
                let anc_canon = dunce::canonicalize(&anc).unwrap_or_else(|_| anc.clone());
                for form in macos_deny_aliases(&anc, &anc_canon) {
                    let Some(escaped) = escape_seatbelt_path(&form) else {
                        anyhow::bail!(
                            "cannot escape ancestor write-deny path {form:?} for Seatbelt"
                        );
                    };
                    emit_seatbelt_ancestor_node_deny(caps, &format!("(literal \"{escaped}\")"))?;
                    rule_paths.push(form);
                }
            }
        }
        let _ = caps.remove_exact_file_caps_for_paths(&rule_paths);
        tracing::info!(
            count = entries.len(),
            "Applied Seatbelt write-deny for EchoAgent-owned direct hook sources"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let _ = (caps, writable_roots);
    }
    Ok(())
}

/// Apply kernel-level deny rules for the given paths.
///
/// On macOS, adds Seatbelt read-deny + write-deny (incl. specific write
/// sub-actions) rules. On Linux, this is a no-op — callers must use bwrap
/// bind-over for read-deny.
#[cfg(all(feature = "enforce", unix))]
pub(crate) fn apply_deny_paths_to_capability_set(
    caps: &mut CapabilitySet,
    deny_paths: &[PathBuf],
) -> anyhow::Result<()> {
    if deny_paths.is_empty() {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        // Every literal path a deny rule was emitted for, so explicit file caps
        // colliding with a denied path can be removed for all alias forms too.
        let mut rule_paths: Vec<PathBuf> = Vec::new();
        for path in deny_paths {
            let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.clone());
            // Dir-ness (subpath vs literal) is decided by existence and applies
            // to all alias forms of this path.
            let use_subpath = deny_path_is_dir(&canonical);
            // The base profile grants `(allow file-read* (subpath "/"))`, which
            // matches every *literal* path. macOS reaches /tmp, /var, /etc via
            // symlinks into /private, so denying only the canonical form is
            // bypassable through the alias — emit a deny for each alias form.
            for form in macos_deny_aliases(path, &canonical) {
                let Some(escaped) = escape_seatbelt_path(&form) else {
                    // Fail CLOSED: a deny path we can't express as a Seatbelt
                    // filter would otherwise be silently unprotected while the
                    // sandbox still reports active. Erroring leaves apply() not
                    // applied so the shell's macOS `!is_applied` guard refuses to
                    // start — matching Linux's any-bind-fails-closed.
                    anyhow::bail!("cannot escape deny path {form:?} for Seatbelt");
                };
                // `literal` for files, `subpath` for dirs, so deny rules are more
                // specific than parent-directory allows.
                let filter = if use_subpath {
                    format!("(subpath \"{escaped}\")")
                } else {
                    format!("(literal \"{escaped}\")")
                };
                emit_seatbelt_deny(caps, &filter)?;
                rule_paths.push(form);
            }
        }
        let _removed = caps.remove_exact_file_caps_for_paths(&rule_paths);
        tracing::info!(
            count = deny_paths.len(),
            "Applied Seatbelt deny rules for sandbox deny paths"
        );
    }

    #[cfg(target_os = "linux")]
    {
        let _ = caps;
        tracing::debug!(
            count = deny_paths.len(),
            "Linux deny paths require bwrap bind-over (applied at process re-exec)"
        );
    }

    Ok(())
}

/// Resolve deny path strings from a profile against the workspace.
///
/// Relative paths are joined with `workspace`. Absolute paths are used as-is.
#[cfg(all(feature = "enforce", unix))]
pub(crate) fn resolve_deny_paths(workspace: &Path, deny: &[PathBuf]) -> Vec<PathBuf> {
    deny.iter()
        .map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                workspace.join(p)
            }
        })
        .collect()
}

/// Resolve, sort, and dedup a profile's deny list into the canonical set of
/// paths to enforce. Shared by the Seatbelt (profiles.rs) and bwrap (lib.rs) sites.
#[cfg(all(feature = "enforce", unix))]
pub(crate) fn effective_deny_paths(workspace: &Path, deny: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = resolve_deny_paths(workspace, deny);
    paths.sort();
    paths.dedup();
    paths
}

/// Resolve already-partitioned EXACT (non-glob) deny entries into bwrap bind
/// strings: resolved against `workspace`, sorted, deduped, stringified. The
/// caller passes the exact slice from `partition_deny_entries`. A Linux bwrap
/// concern (macOS denies via Seatbelt, not path strings) — the exact-path
/// parallel to glob's `expand_deny_globs`, so both deny resolutions live in `deny/`.
#[cfg(all(feature = "enforce", target_os = "linux"))]
pub(crate) fn exact_deny_path_strings(workspace: &Path, exact: &[PathBuf]) -> Vec<String> {
    effective_deny_paths(workspace, exact)
        .into_iter()
        .map(|p| p.display().to_string())
        .collect()
}

/// Whether a deny path should be treated as a directory (Seatbelt `subpath` /
/// bwrap dir-bind) rather than a single file: true for existing directories,
/// false otherwise. Shared by the macOS and Linux deny sites so the two cannot
/// silently diverge.
///
/// Limitation: a non-existent deny path is treated as a single file (macOS emits
/// `(literal …)`); if it is later created as a directory its children are not
/// covered on macOS. Name concrete existing paths to deny a whole directory tree.
#[cfg(all(feature = "enforce", unix))]
pub(crate) fn deny_path_is_dir(canonical: &Path) -> bool {
    canonical.is_dir()
}

#[cfg(test)]
mod tests {
    // All tests here exercise enforce+unix paths; without the gate `super::*`
    // is unused on `--no-default-features`.
    #[cfg(all(feature = "enforce", unix))]
    use super::*;

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn ancestors_pin_under_writable_root_not_home() {
        let tmp = std::env::temp_dir().join(format!(
            "echo-agent-anc-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let echoagent = tmp.join("echoagent");
        let sessions = echoagent.join("sessions");
        let leaf = sessions.join("extra-hooks");
        std::fs::create_dir_all(&leaf).unwrap();
        let ws = tmp.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let roots = [echoagent.clone(), ws.clone()];
        let pin = ancestors_within_writable_roots(&leaf, &roots);
        assert!(
            pin.iter().any(|p| p == &sessions),
            "must pin sessions under ECHO_AGENT_HOME: {pin:?}"
        );
        assert!(
            pin.iter().any(|p| p == &echoagent),
            "must pin ECHO_AGENT_HOME grant root: {pin:?}"
        );
        assert!(
            !pin.iter().any(|p| p == &tmp),
            "must not pin above writable roots: {pin:?}"
        );

        let outside = tmp.join("outside").join("hooks");
        std::fs::create_dir_all(&outside).unwrap();
        let pin_out = ancestors_within_writable_roots(&outside, &roots);
        assert!(
            pin_out.is_empty(),
            "source outside writable roots: leaf-only: {pin_out:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn resolve_deny_paths_relative() {
        let ws = PathBuf::from("/tmp/project");
        let deny = vec![PathBuf::from(".env"), PathBuf::from("/etc/shadow")];
        let resolved = resolve_deny_paths(&ws, &deny);
        assert_eq!(resolved[0], PathBuf::from("/tmp/project/.env"));
        assert_eq!(resolved[1], PathBuf::from("/etc/shadow"));
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "linux"))]
    fn exact_deny_path_strings_resolves_sorts_dedups() {
        let ws = PathBuf::from("/ws");
        // Already-partitioned exact entries (relative + absolute), with a duplicate.
        let exact = vec![
            PathBuf::from("src/server.pem"),
            PathBuf::from(".env"),
            PathBuf::from("/etc/shadow"),
            PathBuf::from(".env"),
        ];
        let paths = exact_deny_path_strings(&ws, &exact);
        assert!(paths.iter().any(|p| p == "/ws/.env"), "{paths:?}");
        assert!(paths.iter().any(|p| p == "/ws/src/server.pem"), "{paths:?}");
        assert!(paths.iter().any(|p| p == "/etc/shadow"), "{paths:?}");
        // Sorted + deduped (the duplicate `.env` collapses to one).
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(paths, sorted, "must be sorted and deduped: {paths:?}");
        assert!(
            !paths.iter().any(|p| p.contains('*')),
            "no globs: {paths:?}"
        );
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn seatbelt_escape_handles_quotes() {
        let p = Path::new("/tmp/foo\"bar");
        let escaped = escape_seatbelt_path(p).unwrap();
        assert!(escaped.contains("\\\""));
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn seatbelt_escape_rejects_control_chars() {
        assert!(escape_seatbelt_path(Path::new("/tmp/a\u{07}b")).is_none());
    }

    #[test]
    #[cfg(all(feature = "enforce", target_os = "macos"))]
    fn macos_deny_aliases_cover_private_symlink() {
        // A canonical /private/tmp denied path must also be denied via its /tmp alias,
        // otherwise the broad read-allow leaves it readable through the alias.
        let canonical = Path::new("/private/tmp/proj/.env");
        let aliases = macos_deny_aliases(canonical, canonical);
        assert!(
            aliases.iter().any(|p| p == Path::new("/tmp/proj/.env")),
            "expected /tmp alias in {aliases:?}"
        );
        assert_eq!(
            toggle_private_prefix(Path::new("/tmp/proj/.env")),
            Some(PathBuf::from("/private/tmp/proj/.env"))
        );
        // Non-firmlink paths (e.g. home credential dirs) have no alias.
        assert_eq!(toggle_private_prefix(Path::new("/Users/x/.ssh")), None);
    }
}
