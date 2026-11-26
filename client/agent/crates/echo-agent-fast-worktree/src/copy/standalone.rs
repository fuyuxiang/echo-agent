//! Post-copy sanitization of a standalone `.git/` so a later `git fetch` cannot
//! recreate the public-base hang (wildcard `origin.fetch` + tens of thousands of
//! remote-tracking refs + a `.git/shallow` graft that is not on HEAD).
//!
//! Prevention only: local filesystem edits, no network.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// First-parent walk cap when deciding whether HEAD itself is shallow.
/// Hitting the cap is treated as uncertain (keep), not as a drop.
const SHALLOW_FIRST_PARENT_CAP: usize = 100_000;

const KEEP_ORIGIN_REFS: &[&str] = &["HEAD", "main", "master"];

const DETACHED_NARROW_FETCH: &str = "+HEAD:refs/remotes/origin/HEAD";

/// Skip rules computed from the *source* `.git/` before the CoW copy.
pub(crate) struct StandaloneCopyFilter {
    pub keep_shallow: bool,
    keep_origin_refs: HashSet<String>,
}

impl StandaloneCopyFilter {
    pub(crate) fn from_git_dir_keeping(git_dir: &Path, extra_origin: &HashSet<String>) -> Self {
        let keep_shallow = match git_dir.join("shallow").is_file() {
            false => true,
            true => !matches!(shallow_consistent_with_head(git_dir), Some(false)),
        };
        let mut keep_origin_refs = origin_ref_allowlist(git_dir);
        keep_origin_refs.extend(extra_origin.iter().cloned());
        Self {
            keep_shallow,
            keep_origin_refs,
        }
    }

    /// Skip a copied entry under `refs/remotes/origin/` that is not allowlisted.
    pub(crate) fn should_skip_origin_remote(&self, rel: &Path) -> bool {
        let Some(name) = origin_remote_rel(rel) else {
            return false;
        };
        !keep_origin_name(&name, &self.keep_origin_refs)
    }
}

/// Narrow `remote.origin.fetch`, drop a HEAD-inconsistent `.git/shallow`, and
/// prune extra `refs/remotes/origin/*` (loose + packed).
///
/// No-ops when `git_dir` is missing or not a directory (linked worktree).
#[cfg(test)]
pub(crate) fn sanitize_standalone_git_dir(git_dir: &Path) -> Result<()> {
    sanitize_standalone_git_dir_keeping(git_dir, &HashSet::new())
}

pub(crate) fn sanitize_standalone_git_dir_keeping(
    git_dir: &Path,
    extra_origin: &HashSet<String>,
) -> Result<()> {
    if !git_dir.is_dir() {
        return Ok(());
    }

    let mut allow = origin_ref_allowlist(git_dir);
    allow.extend(extra_origin.iter().cloned());
    prune_origin_remote_refs(git_dir, &allow)?;
    drop_inconsistent_shallow(git_dir)?;
    rewrite_origin_fetch(git_dir)?;
    Ok(())
}

/// Source-HEAD allowlist plus the dest `git_ref` tip (and its origin
/// upstream). CoW + post-copy sanitize run before checkout, so a non-HEAD
/// dest branch must be kept here — later sanitize can only delete.
pub(crate) fn origin_keep_names_for_git_ref(git_dir: &Path, git_ref: &str) -> HashSet<String> {
    let mut keep = origin_ref_allowlist(git_dir);
    if let Some(name) = checkout_origin_name(git_ref) {
        if let Some(up) = read_upstream_origin_branch(git_dir, &name) {
            keep.insert(up);
        }
        keep.insert(name);
    }
    keep
}

fn checkout_origin_name(git_ref: &str) -> Option<String> {
    let r = git_ref.trim();
    if r.is_empty() || r == "HEAD" || r == "@" {
        return None;
    }
    if let Some(name) = r.strip_prefix("refs/heads/") {
        return valid_checkout_ref_name(name);
    }
    if let Some(name) = r.strip_prefix("refs/remotes/origin/") {
        return valid_checkout_ref_name(name);
    }
    // `git checkout remotes/origin/<name>` is valid; keep the origin tip,
    // not the literal "remotes/origin/<name>" keep-name.
    if let Some(name) = r.strip_prefix("remotes/origin/") {
        return valid_checkout_ref_name(name);
    }
    if let Some(name) = r.strip_prefix("origin/") {
        return valid_checkout_ref_name(name);
    }
    if looks_like_oid(r) {
        return None;
    }
    valid_checkout_ref_name(r)
}

fn valid_checkout_ref_name(name: &str) -> Option<String> {
    if name.is_empty() || name.contains(['*', '\\', '\n', ' ', '\t']) {
        return None;
    }
    Some(name.to_string())
}

fn looks_like_oid(s: &str) -> bool {
    let len = s.len();
    (7..=64).contains(&len) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn narrow_origin_fetch_spec(git_dir: &Path) -> String {
    if let Some(branch) = read_attached_branch(git_dir) {
        return heads_fetch_spec(&branch);
    }
    if let Some(branch) = resolve_origin_default_branch(git_dir) {
        return heads_fetch_spec(&branch);
    }
    DETACHED_NARROW_FETCH.to_string()
}

fn heads_fetch_spec(branch: &str) -> String {
    format!("+refs/heads/{branch}:refs/remotes/origin/{branch}")
}

fn origin_ref_allowlist(git_dir: &Path) -> HashSet<String> {
    let mut keep: HashSet<String> = KEEP_ORIGIN_REFS.iter().map(|s| (*s).to_string()).collect();
    if let Some(branch) = read_attached_branch(git_dir) {
        if let Some(up) = read_upstream_origin_branch(git_dir, &branch) {
            keep.insert(up);
        }
        keep.insert(branch);
    }
    if let Some(default_branch) = resolve_origin_default_branch(git_dir) {
        keep.insert(default_branch);
    }
    keep
}

fn keep_origin_name(name: &str, allow: &HashSet<String>) -> bool {
    allow.contains(name)
        || allow.iter().any(|kept| {
            kept.len() > name.len()
                && kept.starts_with(name)
                && kept.as_bytes().get(name.len()) == Some(&b'/')
        })
}

fn origin_remote_rel(rel: &Path) -> Option<String> {
    let mut comps = rel.components();
    let (a, b, c) = (comps.next()?, comps.next()?, comps.next()?);
    if a.as_os_str() != "refs" || b.as_os_str() != "remotes" || c.as_os_str() != "origin" {
        return None;
    }
    let rest = comps.as_path();
    if rest.as_os_str().is_empty() {
        return None;
    }
    Some(path_to_unix(rest))
}

fn path_to_unix(p: &Path) -> String {
    p.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn read_attached_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    parse_symref(&head, "refs/heads/")
}

fn parse_symref(contents: &str, prefix: &str) -> Option<String> {
    let rest = contents.trim().strip_prefix("ref:")?.trim();
    let name = rest.strip_prefix(prefix)?;
    if name.is_empty() || name.contains(['*', '\\', '\n']) {
        return None;
    }
    Some(name.to_string())
}

fn resolve_origin_default_branch(git_dir: &Path) -> Option<String> {
    let origin_head = git_dir.join("refs/remotes/origin/HEAD");
    if let Some(branch) = std::fs::read_to_string(&origin_head)
        .ok()
        .and_then(|c| parse_symref(&c, "refs/remotes/origin/"))
    {
        return Some(branch);
    }
    for name in ["main", "master"] {
        if origin_ref_exists(git_dir, name) {
            return Some(name.to_string());
        }
    }
    None
}

fn origin_ref_exists(git_dir: &Path, name: &str) -> bool {
    if git_dir.join("refs/remotes/origin").join(name).is_file() {
        return true;
    }
    packed_ref_oid(git_dir, &format!("refs/remotes/origin/{name}")).is_some()
}

fn packed_ref_oid(git_dir: &Path, ref_name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(git_dir.join("packed-refs")).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let oid = parts.next()?;
        let name = parts.next()?;
        if name == ref_name {
            return Some(oid.to_string());
        }
    }
    None
}

fn read_upstream_origin_branch(git_dir: &Path, branch: &str) -> Option<String> {
    let config = std::fs::read_to_string(git_dir.join("config")).ok()?;
    let header = format!("[branch \"{branch}\"]");
    let mut in_branch = false;
    let mut remote = None;
    let mut merge = None;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_branch {
                break;
            }
            in_branch = trimmed == header;
            continue;
        }
        if !in_branch {
            continue;
        }
        if let Some(v) = config_key_value(trimmed, "remote") {
            remote = Some(v.to_string());
        } else if let Some(v) = config_key_value(trimmed, "merge") {
            merge = Some(v.to_string());
        }
    }
    if remote.as_deref() != Some("origin") {
        return None;
    }
    merge
        .as_deref()?
        .strip_prefix("refs/heads/")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn config_key_value<'a>(trimmed: &'a str, key: &str) -> Option<&'a str> {
    let eq = trimmed.find('=')?;
    let name = trimmed[..eq].trim();
    if !name.eq_ignore_ascii_case(key) {
        return None;
    }
    Some(unquote(trimmed[eq + 1..].trim()))
}

fn unquote(value: &str) -> &str {
    if let Some(inner) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return inner;
    }
    if let Some(inner) = value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return inner;
    }
    value
}

/// Always quote. `#` and `;` start gitconfig comments, and both are legal
/// in ref names (`feat#123`, `wip;tmp`).
fn quote_gitconfig_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_fetch_assignment(out: &mut String, indent: &str, spec: &str) {
    out.push_str(indent);
    out.push_str("fetch = ");
    out.push_str(&quote_gitconfig_value(spec));
    out.push('\n');
}

fn rewrite_origin_fetch(git_dir: &Path) -> Result<()> {
    let config_path = git_dir.join("config");
    let Ok(original) = std::fs::read_to_string(&config_path) else {
        return Ok(());
    };
    if !has_origin_remote_section(&original) {
        return Ok(());
    }
    let spec = narrow_origin_fetch_spec(git_dir);
    let Some(rewritten) = rewrite_origin_fetch_in_config(&original, &spec) else {
        return Ok(());
    };
    if rewritten == original {
        return Ok(());
    }
    replace_file(&config_path, &rewritten)
        .with_context(|| format!("failed to rewrite {}", config_path.display()))
}

fn has_origin_remote_section(config: &str) -> bool {
    config.lines().any(|l| is_origin_remote_section(l.trim()))
}

fn is_origin_remote_section(trimmed: &str) -> bool {
    let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(str::trim)
    else {
        return false;
    };
    let Some(rest) = inner.strip_prefix("remote").map(str::trim) else {
        return false;
    };
    rest == "\"origin\"" || rest == "origin"
}

fn is_heads_refspec(spec: &str) -> bool {
    let s = spec.trim().trim_start_matches('+');
    s == "HEAD" || s.starts_with("HEAD:") || s.starts_with("refs/heads/")
}

/// Any wildcard (`+refs/*`, `+refs/heads/*`, pull/tag stars) or heads refspec
/// would let `git fetch` pull remote heads; replace with the narrow spec.
fn should_rewrite_fetch_spec(spec: &str) -> bool {
    spec.contains('*') || is_heads_refspec(spec)
}

fn rewrite_origin_fetch_in_config(config: &str, spec: &str) -> Option<String> {
    if !has_origin_remote_section(config) {
        return None;
    }
    let mut out = String::with_capacity(config.len() + spec.len() + 16);
    let mut in_origin = false;
    let mut wrote_fetch = false;
    let mut saw_fetch_line = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_origin && !wrote_fetch && !saw_fetch_line {
                write_fetch_assignment(&mut out, "\t", spec);
                wrote_fetch = true;
            }
            in_origin = is_origin_remote_section(trimmed);
            if in_origin {
                wrote_fetch = false;
                saw_fetch_line = false;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_origin && let Some(value) = config_key_value(trimmed, "fetch") {
            saw_fetch_line = true;
            if should_rewrite_fetch_spec(value) {
                if !wrote_fetch {
                    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                    write_fetch_assignment(&mut out, &indent, spec);
                    wrote_fetch = true;
                }
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if in_origin && !wrote_fetch && !saw_fetch_line {
        write_fetch_assignment(&mut out, "\t", spec);
    }
    if !config.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    Some(out)
}

fn drop_inconsistent_shallow(git_dir: &Path) -> Result<()> {
    let path = git_dir.join("shallow");
    if !path.is_file() {
        return Ok(());
    }
    if matches!(shallow_consistent_with_head(git_dir), Some(false)) {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove inconsistent {}", path.display()))?;
        tracing::info!(
            git_dir = %git_dir.display(),
            "removed .git/shallow: grafts are unused and every graft parent is in the ODB"
        );
    }
    Ok(())
}

/// `Some(true)` = keep. `Some(false)` = drop. `None` = uncertain (keep).
///
/// Drop only when the graft is proven unused by HEAD *and* every graft and
/// every recorded graft parent exists in the ODB. Walking HEAD to a root is
/// not enough: an orphan HEAD (`gh-pages`) on a real shallow clone must keep
/// the file so `origin/main` does not walk missing parents.
fn shallow_consistent_with_head(git_dir: &Path) -> Option<bool> {
    let grafts = read_shallow_oids(git_dir).ok()?;
    if grafts.is_empty() {
        return Some(true);
    }
    let grafts: HashSet<&str> = grafts.iter().map(String::as_str).collect();

    let repo = gix::open_opts(git_dir, gix::open::Options::isolated()).ok()?;
    if head_first_parent_needs_shallow(&repo, &grafts)? {
        return Some(true);
    }
    if any_graft_hides_missing_parent(&repo, &grafts)? {
        return Some(true);
    }
    Some(false)
}

fn head_first_parent_needs_shallow(repo: &gix::Repository, grafts: &HashSet<&str>) -> Option<bool> {
    let mut commit = repo.head_commit().ok()?;
    for _ in 0..SHALLOW_FIRST_PARENT_CAP {
        let id_hex = commit.id().to_string();
        if grafts.contains(id_hex.as_str()) {
            return Some(true);
        }
        let Some(parent_id) = commit.parent_ids().next() else {
            return Some(false);
        };
        let parent_oid = parent_id.detach();
        let parent_hex = parent_oid.to_string();
        if grafts.contains(parent_hex.as_str()) {
            return Some(true);
        }
        match repo.find_commit(parent_oid) {
            Ok(parent) => commit = parent,
            Err(_) => return Some(true),
        }
    }
    None
}

fn any_graft_hides_missing_parent(repo: &gix::Repository, grafts: &HashSet<&str>) -> Option<bool> {
    for graft in grafts {
        let oid = gix::ObjectId::from_hex(graft.as_bytes()).ok()?;
        let commit = match repo.find_commit(oid) {
            Ok(commit) => commit,
            Err(_) => return Some(true),
        };
        for parent in commit.parent_ids() {
            if repo.find_commit(parent.detach()).is_err() {
                return Some(true);
            }
        }
    }
    Some(false)
}

fn read_shallow_oids(git_dir: &Path) -> Result<Vec<String>> {
    let path = git_dir.join("shallow");
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_ascii_lowercase)
        .collect())
}

fn prune_origin_remote_refs(git_dir: &Path, allow: &HashSet<String>) -> Result<()> {
    let origin = git_dir.join("refs/remotes/origin");
    if origin.is_dir() {
        prune_origin_loose(&origin, Path::new(""), allow)?;
    }
    filter_packed_refs(git_dir, allow)
}

fn prune_origin_loose(dir: &Path, rel: &Path, allow: &HashSet<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", dir.display()));
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let name = entry.file_name();
        let child_rel = rel.join(&name);
        let child_name = path_to_unix(&child_rel);
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat {}", path.display()))?;
        if file_type.is_dir() {
            prune_origin_loose(&path, &child_rel, allow)?;
            let _ = std::fs::remove_dir(&path);
        } else if !keep_origin_name(&child_name, allow) {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

fn filter_packed_refs(git_dir: &Path, allow: &HashSet<String>) -> Result<()> {
    let path = git_dir.join("packed-refs");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut out = String::with_capacity(contents.len());
    let mut keep_peeled = false;
    let mut changed = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if trimmed.starts_with('^') {
            if keep_peeled {
                out.push_str(line);
                out.push('\n');
            } else {
                changed = true;
            }
            continue;
        }
        let ref_name = trimmed.split_whitespace().nth(1);
        let keep = match ref_name.and_then(|n| n.strip_prefix("refs/remotes/origin/")) {
            Some(name) => keep_origin_name(name, allow),
            None => true,
        };
        if keep {
            keep_peeled = true;
            out.push_str(line);
            out.push('\n');
        } else {
            keep_peeled = false;
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    if !contents.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    replace_file(&path, &out).with_context(|| format!("failed to rewrite {}", path.display()))
}

fn replace_file(path: &Path, contents: &str) -> Result<()> {
    let tmp = tmp_sibling(path);
    std::fs::write(&tmp, contents).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        format!(
            "failed to replace {} with {}",
            path.display(),
            tmp.display()
        )
    })
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("file"))
        .to_os_string();
    name.push(".tmp-standalone");
    path.with_file_name(name)
}

#[cfg(test)]
#[path = "standalone_tests.rs"]
mod tests;
