use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
#[cfg(any(test, feature = "metadata"))]
use std::time::Duration;

use gix::ObjectId;

use super::probe::{ProbeError, oids_to_stdin, run_probe};
use super::safety::{self, KeepReason, Safety};

const RECLAIMED: &str = "refs/echoagent/reclaimed";

#[cfg(any(test, feature = "metadata"))]
pub const RECLAIMED_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[must_use]
#[derive(Debug)]
pub enum Reclaim {
    Now { named: usize },
    Keep(KeepReason),
    Unnamed(std::io::Error),
}

#[cfg(any(test, feature = "metadata"))]
pub fn reclaimable_within(worktree: &Path, surviving: Option<&Path>, timeout: Duration) -> Reclaim {
    answer(
        worktree,
        safety::safe_to_delete_named_worktree(worktree, surviving, None, Some(timeout)),
    )
}

pub fn reclaimable_after_snapshot(
    worktree: &Path,
    surviving: Option<&Path>,
    snapshot: &str,
) -> Reclaim {
    answer(
        worktree,
        safety::safe_to_delete_named_worktree(worktree, surviving, Some(snapshot), None),
    )
}

fn answer(worktree: &Path, safety: Safety) -> Reclaim {
    match safety {
        Safety::Keep(reason) => Reclaim::Keep(reason),
        Safety::Delete => match name_discarded_commits(worktree) {
            Ok(named) => Reclaim::Now { named },
            Err(error) => Reclaim::Unnamed(error),
        },
    }
}

fn name_discarded_commits(worktree: &Path) -> std::io::Result<usize> {
    let repo = match gix::open(worktree) {
        Ok(repo) => repo,
        // Only a definitively-absent `.git` means "nothing to name"; a stat
        // error is "couldn't tell" and must surface as Err (caller keeps).
        Err(_) if safety::git_entry_definitely_absent(worktree) => return Ok(0),
        Err(error) => return Err(std::io::Error::other(error)),
    };
    if repo.common_dir() == repo.git_dir() {
        return Ok(0);
    }
    let discarded = safety::collect_reflog_tips(repo.git_dir())?;
    if discarded.is_empty() {
        return Ok(0);
    }
    let tips = unreachable_tips(worktree, &discarded)?;
    if tips.is_empty() {
        return Ok(0);
    }
    let reclaimed_at = crate::time::epoch_secs();
    let name = ref_name_for(worktree);
    let mut updates = Vec::new();
    for id in &tips {
        let _ = writeln!(
            updates,
            "update {RECLAIMED}/{name}/{reclaimed_at}/{id} {id}"
        );
    }
    run_git(worktree, &["update-ref", "--stdin"], updates)?;
    tracing::info!(
        path = %worktree.display(),
        named = tips.len(),
        "named reflog-only commits under refs/echoagent/reclaimed"
    );
    Ok(tips.len())
}

fn unreachable_tips(
    worktree: &Path,
    discarded: &HashSet<ObjectId>,
) -> std::io::Result<Vec<ObjectId>> {
    let revisions = oids_to_stdin(discarded.iter().copied());
    let output = run_git(
        worktree,
        &[
            "rev-list",
            "--parents",
            "--ignore-missing",
            "--stdin",
            "--not",
            "--all",
            "--",
        ],
        revisions,
    )?;
    let mut tips = Vec::new();
    let mut reached = HashSet::new();
    for line in output.split(|byte| *byte == b'\n') {
        let mut ids = line
            .split(|byte| *byte == b' ')
            .filter_map(|id| ObjectId::from_hex(id).ok());
        let Some(commit) = ids.next() else {
            continue;
        };
        reached.extend(ids);
        if discarded.contains(&commit) {
            tips.push(commit);
        }
    }
    tips.retain(|id| !reached.contains(id));
    Ok(tips)
}

#[cfg(any(test, feature = "metadata"))]
pub fn collect_reclaimed_names(repo: &Path, lifetime: Duration) -> std::io::Result<usize> {
    let listed = run_git(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            RECLAIMED,
        ],
        Vec::new(),
    )?;
    let mut named: Vec<(ObjectId, i64, String)> = Vec::new();
    for line in String::from_utf8_lossy(&listed).lines() {
        let mut fields = line.splitn(2, ' ');
        let (Some(id), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Ok(id) = ObjectId::from_hex(id.as_bytes()) else {
            continue;
        };
        let Some(at) = reclaimed_at(name) else {
            continue;
        };
        named.push((id, at, name.to_string()));
    }
    if named.is_empty() {
        return Ok(0);
    }
    let still_alone = commits_nothing_else_reaches(repo, &named)?;
    let expired = expiry(lifetime);
    let mut drops = Vec::new();
    let mut dropped = 0usize;
    for (id, at, name) in &named {
        if still_alone.contains(id) && *at >= expired {
            continue;
        }
        let _ = writeln!(drops, "delete {name} {id}");
        dropped += 1;
    }
    if drops.is_empty() {
        return Ok(0);
    }
    run_git(repo, &["update-ref", "--stdin"], drops)?;
    tracing::info!(
        path = %repo.display(),
        dropped,
        "dropped reclaimed refs whose commits are reachable or expired"
    );
    Ok(dropped)
}

#[cfg(any(test, feature = "metadata"))]
fn reclaimed_at(refname: &str) -> Option<i64> {
    let rest = refname.strip_prefix(RECLAIMED)?.strip_prefix('/')?;
    let mut parts = rest.split('/');
    let _worktree = parts.next()?;
    let timestamp = parts.next()?.parse::<i64>().ok()?;
    let _id = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(timestamp)
}

#[cfg(any(test, feature = "metadata"))]
fn expiry(lifetime: Duration) -> i64 {
    crate::time::epoch_secs().saturating_sub(i64::try_from(lifetime.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(any(test, feature = "metadata"))]
fn commits_nothing_else_reaches(
    repo: &Path,
    named: &[(ObjectId, i64, String)],
) -> std::io::Result<HashSet<ObjectId>> {
    let revisions = oids_to_stdin(named.iter().map(|(id, _, _)| *id));
    let output = run_git(
        repo,
        &[
            "rev-list",
            "--ignore-missing",
            "--stdin",
            "--not",
            &format!("--exclude={RECLAIMED}/*"),
            "--all",
            "--",
        ],
        revisions,
    )?;
    Ok(output
        .split(|byte| *byte == b'\n')
        .filter_map(|line| ObjectId::from_hex(line).ok())
        .collect())
}

fn ref_name_for(worktree: &Path) -> String {
    let name: String = worktree
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches('-');
    if name.is_empty() {
        "worktree".to_string()
    } else {
        name.to_string()
    }
}

fn run_git(at: &Path, args: &[&str], stdin: Vec<u8>) -> std::io::Result<Vec<u8>> {
    let what = args.first().copied().unwrap_or_default();
    let mut command = safety::probe_command(at);
    command.args(args);
    match run_probe(at, command, stdin, what) {
        Ok(output) => Ok(output.stdout),
        Err(ProbeError::DidNotRun(error)) => Err(error),
        Err(ProbeError::Failed { stderr }) => Err(std::io::Error::other(format!(
            "git {what} failed: {}",
            String::from_utf8_lossy(&stderr)
        ))),
    }
}

#[cfg(test)]
#[path = "reclaimed_tests.rs"]
mod tests;
