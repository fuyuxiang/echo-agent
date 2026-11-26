//! Resume-by-title selection shared by startup paths.
//!
//! The composition root pins an explicit non-id resume target to its
//! canonical session id BEFORE the irreversible OS sandbox
//! ([`super::cli::PagerArgs::pin_local_resume_target`]), so the saved-profile
//! peek and materialization act on one immutable target instead of racing a
//! concurrent rename between two title lookups. Materialization keeps
//! [`select_by_title`] as the authoritative error source (ambiguity /
//! no-match) and as a fallback for callers that bypass pinning.

use echo_agent_runtime::session::persistence::Summary;

/// UUID-shaped resume args always take the id path, even when no such id
/// exists and a session is titled with that exact UUID.
pub(crate) fn is_uuid_shaped(arg: &str) -> bool {
    uuid::Uuid::try_parse(arg).is_ok()
}

/// Canonical key for title equality: trimmed `str::to_lowercase`. Plain
/// case-insensitive equality, not full Unicode caseless matching.
fn title_key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Hint appended to every terminal failure for a non-id resume target, so the
/// title miss stays visible even when remote restore produces the final error.
/// Debug formatting: the arg is arbitrary user text.
pub(crate) fn title_miss_hint(arg: &str) -> String {
    format!(
        "no session id or title matched {arg:?} for this directory; \
         try `echoagent sessions search {arg:?}`"
    )
}

/// Select the local session a resume arg names by title.
///
/// - `Ok(None)`: UUID-shaped or blank arg, or no title matched — the caller
///   keeps id-miss behavior.
/// - `Ok(Some)`: exactly one match, or a sole manual `/rename` among
///   duplicates (explicit user intent beats colliding auto titles).
/// - `Err`: ambiguous — never silently pick one, headless scripts need
///   determinism. Candidate titles are Debug-escaped: `/rename` accepts
///   arbitrary text, and raw control characters would corrupt the listing.
pub(crate) fn select_by_title<'a>(
    arg: &str,
    summaries: &'a [Summary],
) -> anyhow::Result<Option<&'a Summary>> {
    if is_uuid_shaped(arg) {
        return Ok(None);
    }
    let needle = title_key(arg);
    if needle.is_empty() {
        return Ok(None);
    }
    let matches: Vec<&Summary> = summaries
        .iter()
        .filter(|s| title_key(s.display_title()) == needle)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(*only)),
        _ => {
            let manual: Vec<&&Summary> = matches
                .iter()
                .filter(|s| {
                    s.manual_title_opt()
                        .is_some_and(|t| title_key(&t) == needle)
                })
                .collect();
            if let [only] = manual.as_slice() {
                return Ok(Some(**only));
            }
            let listing = matches
                .iter()
                .map(|s| format!("  {}  {:?}", s.info.id, s.display_title()))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!(
                "Multiple sessions match title {:?}:\n{listing}\n\
                 Resume by session id instead: echoagent --resume <session-id>",
                arg.trim()
            );
        }
    }
}

/// Outcome of the pre-sandbox resolution of an explicit resume arg.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PinnedResumeTarget {
    /// Nothing local resolved (UUID-shaped, no cwd, junk, or ambiguous
    /// title): leave the raw arg alone — materialization owns the
    /// authoritative error / remote path.
    Unresolved,
    /// Resolved as a local id (possibly the restored child of a remote id).
    Id(String),
    /// Resolved by title to this session. The selected summary's persisted
    /// sandbox profile rides along: re-deriving it from the id is ambiguous
    /// when a legacy id is duplicated across cwd dirs.
    Title {
        id: String,
        sandbox_profile: Option<String>,
    },
}

impl PinnedResumeTarget {
    pub(crate) fn id(self) -> Option<String> {
        match self {
            Self::Unresolved => None,
            Self::Id(id) | Self::Title { id, .. } => Some(id),
        }
    }
}

/// Resolve an explicit resume arg to a pinned local session id before the
/// (irreversible) OS sandbox: the saved-profile peek and materialization must
/// consume one immutable target, not re-run title selection against mutable
/// summaries. Id lookups stay authoritative (same order as
/// `resolve_existing_session`), preserving the restored-child id so the peek
/// cannot drift to a same-id session in another cwd. Errs on a listing
/// failure (fail closed instead of guessing) and on ambiguity, which must
/// surface before the sandbox rather than after it.
pub(crate) fn presandbox_resume_target(
    arg: &str,
    cwd: Option<&str>,
) -> anyhow::Result<PinnedResumeTarget> {
    if is_uuid_shaped(arg) {
        return Ok(PinnedResumeTarget::Unresolved);
    }
    let Some(cwd) = cwd else {
        return Ok(PinnedResumeTarget::Unresolved);
    };
    if let Some(local_id) = echo_agent_runtime::session::resolve_local_session(arg, cwd) {
        return Ok(PinnedResumeTarget::Id(local_id));
    }
    if echo_agent_runtime::session::resolve_local_session_any_cwd(arg).is_some() {
        return Ok(PinnedResumeTarget::Id(arg.to_string()));
    }
    let summaries = echo_agent_runtime::session::persistence::local_summaries_for_cwd_sync(cwd)
        .map_err(|e| {
            anyhow::anyhow!("failed to list local sessions while resolving --resume {arg:?}: {e}")
        })?;
    Ok(select_by_title(arg, &summaries)?
        .map(|s| PinnedResumeTarget::Title {
            id: s.info.id.to_string(),
            sandbox_profile: s.sandbox_profile.clone(),
        })
        .unwrap_or(PinnedResumeTarget::Unresolved))
}

/// Failure message for a worktree resume. `local_miss_target` is `Some(arg)`
/// only when materialization deferred exactly this target after missing local
/// id/title resolution — provenance is threaded, never inferred from id
/// shape, so a resolved legacy non-UUID id gets no false no-match hint.
/// `detail` must already be user-sanitized: sanitizing the composed message
/// instead would collapse disk-full chains whole and erase the appended hint.
pub(crate) fn worktree_resume_failure_message(
    local_miss_target: Option<&str>,
    detail: &str,
) -> String {
    let msg = format!("couldn't resume worktree session: {detail}");
    match local_miss_target {
        Some(target) => format!("{msg}; {}", title_miss_hint(target)),
        None => msg,
    }
}

#[cfg(test)]
#[path = "session_title_resolve_tests.rs"]
mod tests;
