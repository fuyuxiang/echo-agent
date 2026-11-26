//! Pure resolution logic for `echoagent plugin install <name>` marketplace refs.

use crate::types::{MarketplaceEntry, MarketplaceSource, SourceKind};
use crate::{canonical_github_owner_repo, is_official_source_url};

/// A parsed marketplace install ref: a plugin `name` with an optional source
/// `qualifier` (`owner/repo` for git, `local/<slug>` for local sources).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketplaceRef {
    pub name: String,
    pub qualifier: Option<String>,
}

/// Recognize `<name>` / `<name>@<qualifier>` install args, leaving git URLs,
/// GitHub shorthand, and local paths (including Windows paths) for the existing
/// parser.
pub fn parse_marketplace_ref(arg: &str) -> Option<MarketplaceRef> {
    if arg.contains("://") || arg.starts_with("git@") {
        return None;
    }
    if arg.starts_with('/')
        || arg.starts_with('\\')
        || arg.starts_with('.')
        || arg.starts_with('~')
        || is_windows_drive_path(arg)
    {
        return None;
    }
    if arg.contains('#') {
        return None;
    }
    let (name, qualifier) = match arg.split_once('@') {
        Some((name, qualifier)) => (name, Some(qualifier)),
        None => (arg, None),
    };
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }
    if qualifier.is_some_and(|q| q.trim().is_empty()) {
        return None;
    }
    Some(MarketplaceRef {
        name: name.to_string(),
        qualifier: qualifier.map(str::to_string),
    })
}

fn is_windows_drive_path(arg: &str) -> bool {
    let bytes = arg.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Lowercase a source name and turn whitespace runs into single hyphens.
pub fn slugify(name: &str) -> String {
    name.to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// The qualifier a user would type to pin this source: `owner/repo` for a
/// GitHub git source, `git/<slug>` for a non-GitHub git source, `local/<slug>`
/// for a local source.
pub fn addressable_qualifier(source: &MarketplaceSource) -> String {
    match &source.kind {
        SourceKind::Git { url, .. } => canonical_github_owner_repo(url)
            .unwrap_or_else(|| format!("git/{}", slugify(&source.name))),
        SourceKind::Local { .. } => format!("local/{}", slugify(&source.name)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualifierResolveError {
    Unknown,
    /// More than one registered source matches; payload is their indices.
    Ambiguous(Vec<usize>),
}

/// Resolve a qualifier to exactly one registered source index.
///
/// A bare `owner/repo` matches GitHub git sources. `local/<slug>` and
/// `git/<slug>` match local/git sources by slugified name, and both also keep
/// the `owner/repo` interpretation so a GitHub source owned by `git`/`local`
/// still resolves. A qualifier also matches a source's registered `name`
/// (exactly, or slugified): `<plugin>@<marketplace-name>` is the only pin for
/// non-github.com hosts (e.g. GitHub Enterprise) that have no `owner/repo`
/// form. Matches spanning more than one source surface as
/// [`QualifierResolveError::Ambiguous`].
pub fn resolve_qualified_source(
    qualifier: &str,
    sources: &[MarketplaceSource],
) -> Result<usize, QualifierResolveError> {
    let want = normalize_owner_repo_qualifier(qualifier);
    let local_slug = qualifier.strip_prefix("local/");
    let git_slug = qualifier.strip_prefix("git/");

    let matched: Vec<usize> = sources
        .iter()
        .enumerate()
        .filter(|(_, source)| {
            let owner_repo = match &source.kind {
                SourceKind::Git { url, .. } => {
                    canonical_github_owner_repo(url).as_deref() == Some(want.as_str())
                }
                SourceKind::Local { .. } => false,
            };
            let local = local_slug.is_some_and(|slug| {
                matches!(&source.kind, SourceKind::Local { .. }) && slugify(&source.name) == slug
            });
            let git = git_slug.is_some_and(|slug| {
                matches!(&source.kind, SourceKind::Git { .. }) && slugify(&source.name) == slug
            });
            let by_name = source.name == qualifier || slugify(&source.name) == slugify(qualifier);
            owner_repo || local || git || by_name
        })
        .map(|(index, _)| index)
        .collect();

    match matched.as_slice() {
        [] => Err(QualifierResolveError::Unknown),
        [index] => Ok(*index),
        _ => Err(QualifierResolveError::Ambiguous(matched)),
    }
}

fn normalize_owner_repo_qualifier(qualifier: &str) -> String {
    let trimmed = qualifier.trim();
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    trimmed.to_ascii_lowercase()
}

/// One scanned marketplace entry tagged with the source it came from.
pub struct ScannedEntry<'a> {
    pub source: &'a MarketplaceSource,
    pub entry: &'a MarketplaceEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BareNameSelection {
    /// Index into the scanned slice of the entry to install.
    pub chosen: usize,
    /// How many other copies of the name exist (non-zero only when official
    /// priority broke a tie).
    pub other_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BareNameError {
    NotFound,
    /// Several sources provide the name and none is uniquely official; payload
    /// is the matching indices into the scanned slice.
    Ambiguous {
        matched: Vec<usize>,
    },
}

/// Choose which scanned entry to install for a bare `<name>` (case-insensitive).
///
/// One match wins outright. With several matches, a single official-source copy
/// wins (reporting the others); otherwise the result is ambiguous.
pub fn select_bare_name(
    name: &str,
    scanned: &[ScannedEntry<'_>],
) -> Result<BareNameSelection, BareNameError> {
    let matched: Vec<usize> = scanned
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.entry.name.eq_ignore_ascii_case(name))
        .map(|(index, _)| index)
        .collect();

    match matched.as_slice() {
        [] => Err(BareNameError::NotFound),
        [index] => Ok(BareNameSelection {
            chosen: *index,
            other_count: 0,
        }),
        _ => {
            let official: Vec<usize> = matched
                .iter()
                .copied()
                .filter(|&index| match &scanned[index].source.kind {
                    SourceKind::Git { url, .. } => is_official_source_url(url),
                    SourceKind::Local { .. } => false,
                })
                .collect();
            match official.as_slice() {
                [index] => Ok(BareNameSelection {
                    chosen: *index,
                    other_count: matched.len() - 1,
                }),
                _ => Err(BareNameError::Ambiguous { matched }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git_source(name: &str, url: &str) -> MarketplaceSource {
        MarketplaceSource {
            name: name.to_string(),
            kind: SourceKind::Git {
                url: url.to_string(),
                branch: None,
            },
        }
    }

    fn local_source(name: &str, path: &str) -> MarketplaceSource {
        MarketplaceSource {
            name: name.to_string(),
            kind: SourceKind::Local {
                path: PathBuf::from(path),
            },
        }
    }

    fn entry(name: &str) -> MarketplaceEntry {
        MarketplaceEntry {
            name: name.to_string(),
            version: None,
            description: None,
            category: None,
            author: None,
            tags: Vec::new(),
            keywords: Vec::new(),
            domains: Vec::new(),
            homepage: None,
            relative_path: format!("plugins/{name}"),
            skill_count: 0,
            has_hooks: false,
            has_agents: false,
            has_mcp: false,
            remote_url: None,
            remote_ref: None,
            remote_sha: None,
            remote_subdir: None,
            components: None,
        }
    }

    fn scanned_entries<'a>(
        pairs: &'a [(MarketplaceSource, MarketplaceEntry)],
    ) -> Vec<ScannedEntry<'a>> {
        pairs
            .iter()
            .map(|(source, entry)| ScannedEntry { source, entry })
            .collect()
    }

    #[test]
    fn parse_bare_name() {
        assert_eq!(
            parse_marketplace_ref("sentry"),
            Some(MarketplaceRef {
                name: "sentry".into(),
                qualifier: None,
            })
        );
    }

    #[test]
    fn parse_name_with_owner_repo_qualifier() {
        assert_eq!(
            parse_marketplace_ref("sentry@fuyuxiang/echo-agent-desktop"),
            Some(MarketplaceRef {
                name: "sentry".into(),
                qualifier: Some("fuyuxiang/echo-agent-desktop".into()),
            })
        );
    }

    #[test]
    fn parse_name_with_local_slug_qualifier() {
        assert_eq!(
            parse_marketplace_ref("sentry@local/local-dev"),
            Some(MarketplaceRef {
                name: "sentry".into(),
                qualifier: Some("local/local-dev".into()),
            })
        );
    }

    #[test]
    fn parse_rejects_git_shorthand_with_ref() {
        assert_eq!(parse_marketplace_ref("owner/repo@v1.0"), None);
    }

    #[test]
    fn parse_rejects_urls_and_local_paths() {
        assert_eq!(parse_marketplace_ref("https://github.com/owner/repo"), None);
        assert_eq!(parse_marketplace_ref("git@github.com:owner/repo.git"), None);
        assert_eq!(parse_marketplace_ref("./x"), None);
        assert_eq!(parse_marketplace_ref("/abs"), None);
        assert_eq!(parse_marketplace_ref("~/x"), None);
    }

    #[test]
    fn parse_rejects_windows_paths() {
        assert_eq!(parse_marketplace_ref(r"C:\Users\me\plugin"), None);
        assert_eq!(parse_marketplace_ref("C:/Users/me/plugin"), None);
        assert_eq!(parse_marketplace_ref(r"\\server\share\plugin"), None);
        assert_eq!(parse_marketplace_ref(r"sub\plugin"), None);
    }

    #[test]
    fn parse_rejects_trailing_or_whitespace_qualifier() {
        assert_eq!(parse_marketplace_ref("sentry@"), None);
        assert_eq!(parse_marketplace_ref("sentry@   "), None);
    }

    #[test]
    fn parse_rejects_leading_at() {
        assert_eq!(parse_marketplace_ref("@foo"), None);
    }

    #[test]
    fn parse_rejects_fragment() {
        assert_eq!(parse_marketplace_ref("sentry#sub"), None);
        assert_eq!(
            parse_marketplace_ref("sentry@example/marketplace#sub"),
            None
        );
    }

    #[test]
    fn parse_splits_on_first_at_only() {
        assert_eq!(
            parse_marketplace_ref("a@b@c"),
            Some(MarketplaceRef {
                name: "a".into(),
                qualifier: Some("b@c".into()),
            })
        );
    }

    #[test]
    fn slugify_lowercases_and_hyphenates_spaces() {
        assert_eq!(slugify("Local Dev"), "local-dev");
        assert_eq!(slugify("EchoAgent Official"), "echoagent-official");
    }

    #[test]
    fn addressable_qualifier_git_and_local() {
        assert_eq!(
            addressable_qualifier(&git_source(
                "x",
                "https://github.com/fuyuxiang/echo-agent-desktop.git"
            )),
            "fuyuxiang/echo-agent-desktop"
        );
        assert_eq!(
            addressable_qualifier(&local_source("Local Dev", "/tmp/p")),
            "local/local-dev"
        );
    }

    #[test]
    fn addressable_qualifier_non_github_git_uses_git_slug() {
        assert_eq!(
            addressable_qualifier(&git_source(
                "Self Hosted",
                "https://git.example.com/org/repo.git"
            )),
            "git/self-hosted"
        );
    }

    #[test]
    fn resolve_qualifier_matches_git_owner_repo_across_url_forms() {
        for url in [
            "https://github.com/fuyuxiang/echo-agent-desktop.git",
            "git@github.com:fuyuxiang/echo-agent-desktop.git",
            "ssh://git@github.com/fuyuxiang/echo-agent-desktop",
            "https://GitHub.com/Fuyuxiang/Echo-Agent-Desktop",
        ] {
            let sources = [git_source("src", url)];
            assert_eq!(
                resolve_qualified_source("fuyuxiang/echo-agent-desktop", &sources),
                Ok(0),
                "url: {url}"
            );
        }
    }

    #[test]
    fn resolve_qualifier_normalizes_dot_git_in_qualifier() {
        let sources = [git_source(
            "src",
            "https://github.com/fuyuxiang/echo-agent-desktop",
        )];
        assert_eq!(
            resolve_qualified_source("fuyuxiang/echo-agent-desktop.git", &sources),
            Ok(0)
        );
    }

    #[test]
    fn resolve_qualifier_matches_local_by_slug() {
        let sources = [
            git_source(
                "EchoAgent Official",
                "https://github.com/fuyuxiang/echo-agent-desktop.git",
            ),
            local_source("Local Dev", "/tmp/plugins"),
        ];
        assert_eq!(resolve_qualified_source("local/local-dev", &sources), Ok(1));
    }

    #[test]
    fn resolve_qualifier_matches_non_github_git_by_slug() {
        let sources = [
            git_source(
                "EchoAgent Official",
                "https://github.com/fuyuxiang/echo-agent-desktop.git",
            ),
            git_source("Self Hosted", "https://git.example.com/org/repo.git"),
        ];
        assert_eq!(resolve_qualified_source("git/self-hosted", &sources), Ok(1));
    }

    #[test]
    fn resolve_qualifier_github_owner_named_git_round_trips() {
        let sources = [git_source("X", "https://github.com/git/tools.git")];
        assert_eq!(addressable_qualifier(&sources[0]), "git/tools");
        assert_eq!(resolve_qualified_source("git/tools", &sources), Ok(0));
    }

    #[test]
    fn resolve_qualifier_git_prefix_collision_is_ambiguous() {
        let sources = [
            git_source("X", "https://github.com/git/tools.git"),
            git_source("Tools", "https://git.example.com/org/tools.git"),
        ];
        assert_eq!(
            resolve_qualified_source("git/tools", &sources),
            Err(QualifierResolveError::Ambiguous(vec![0, 1]))
        );
    }

    #[test]
    fn resolve_qualifier_unknown_for_git_and_local() {
        let sources = [
            git_source(
                "EchoAgent Official",
                "https://github.com/fuyuxiang/echo-agent-desktop.git",
            ),
            local_source("Local Dev", "/tmp/plugins"),
        ];
        assert_eq!(
            resolve_qualified_source("other/repo", &sources),
            Err(QualifierResolveError::Unknown)
        );
        assert_eq!(
            resolve_qualified_source("local/nope", &sources),
            Err(QualifierResolveError::Unknown)
        );
    }

    #[test]
    fn resolve_qualifier_ambiguous_lists_all_matching_indices() {
        let sources = [
            git_source(
                "Mirror A",
                "https://github.com/fuyuxiang/echo-agent-desktop.git",
            ),
            git_source(
                "Mirror B",
                "git@github.com:fuyuxiang/echo-agent-desktop.git",
            ),
        ];
        assert_eq!(
            resolve_qualified_source("fuyuxiang/echo-agent-desktop", &sources),
            Err(QualifierResolveError::Ambiguous(vec![0, 1]))
        );
    }

    #[test]
    fn resolve_qualifier_matches_marketplace_name_for_non_github_host() {
        let sources = [git_source(
            "internal-tools",
            "git@github.example.com:acme/internal-tools.git",
        )];
        assert_eq!(resolve_qualified_source("internal-tools", &sources), Ok(0));
    }

    #[test]
    fn resolve_qualifier_matches_name_case_and_space_insensitively() {
        let sources = [git_source(
            "Internal Tools",
            "git@github.example.com:acme/internal-tools.git",
        )];
        assert_eq!(resolve_qualified_source("Internal Tools", &sources), Ok(0));
        assert_eq!(resolve_qualified_source("internal-tools", &sources), Ok(0));
    }

    #[test]
    fn resolve_qualifier_matches_local_source_by_name() {
        let sources = [local_source("Local Dev", "/tmp/plugins")];
        assert_eq!(resolve_qualified_source("Local Dev", &sources), Ok(0));
        assert_eq!(resolve_qualified_source("local-dev", &sources), Ok(0));
    }

    #[test]
    fn resolve_qualifier_duplicate_names_are_ambiguous() {
        let sources = [
            git_source("dup", "https://git.a.example.com/o/r.git"),
            git_source("dup", "https://git.b.example.com/o/r.git"),
        ];
        assert_eq!(
            resolve_qualified_source("dup", &sources),
            Err(QualifierResolveError::Ambiguous(vec![0, 1]))
        );
    }

    #[test]
    fn resolve_qualifier_name_vs_other_source_owner_repo_is_ambiguous() {
        let sources = [
            git_source(
                "EchoAgent Official",
                "https://github.com/fuyuxiang/echo-agent-desktop.git",
            ),
            git_source(
                "fuyuxiang/echo-agent-desktop",
                "git@github.example.com:mirror/echoagent.git",
            ),
        ];
        assert_eq!(
            resolve_qualified_source("fuyuxiang/echo-agent-desktop", &sources),
            Err(QualifierResolveError::Ambiguous(vec![0, 1]))
        );
    }

    #[test]
    fn resolve_qualifier_name_and_owner_repo_same_source_resolves() {
        let sources = [git_source(
            "fuyuxiang/echo-agent-desktop",
            "https://github.com/fuyuxiang/echo-agent-desktop.git",
        )];
        assert_eq!(
            resolve_qualified_source("fuyuxiang/echo-agent-desktop", &sources),
            Ok(0)
        );
    }

    #[test]
    fn resolve_qualifier_unknown_name_still_unknown() {
        let sources = [git_source(
            "internal-tools",
            "git@github.example.com:x/y.git",
        )];
        assert_eq!(
            resolve_qualified_source("nope", &sources),
            Err(QualifierResolveError::Unknown)
        );
    }

    #[test]
    fn bare_name_single_match_selected() {
        let pairs = [(
            git_source("src", "https://github.com/o/r.git"),
            entry("sentry"),
        )];
        let scanned = scanned_entries(&pairs);
        assert_eq!(
            select_bare_name("sentry", &scanned),
            Ok(BareNameSelection {
                chosen: 0,
                other_count: 0,
            })
        );
    }

    #[test]
    fn bare_name_matches_case_insensitively() {
        let pairs = [(
            git_source("src", "https://github.com/o/r.git"),
            entry("Sentry"),
        )];
        let scanned = scanned_entries(&pairs);
        assert_eq!(
            select_bare_name("sentry", &scanned),
            Ok(BareNameSelection {
                chosen: 0,
                other_count: 0,
            })
        );
    }

    #[test]
    fn bare_name_official_priority_when_duplicate_in_official_and_third_party() {
        let pairs = [
            (
                git_source("Third Party", "https://github.com/acme/marketplace.git"),
                entry("sentry"),
            ),
            (
                git_source(
                    "EchoAgent Official",
                    "https://github.com/fuyuxiang/echo-agent-desktop.git",
                ),
                entry("sentry"),
            ),
        ];
        let scanned = scanned_entries(&pairs);
        assert_eq!(
            select_bare_name("sentry", &scanned),
            Ok(BareNameSelection {
                chosen: 1,
                other_count: 1,
            })
        );
    }

    #[test]
    fn bare_name_ambiguous_when_no_official_match() {
        let pairs = [
            (
                git_source("Third Party A", "https://github.com/acme/a.git"),
                entry("sentry"),
            ),
            (
                git_source("Third Party B", "https://github.com/acme/b.git"),
                entry("sentry"),
            ),
        ];
        let scanned = scanned_entries(&pairs);
        assert_eq!(
            select_bare_name("sentry", &scanned),
            Err(BareNameError::Ambiguous {
                matched: vec![0, 1]
            })
        );
    }

    #[test]
    fn bare_name_ambiguous_when_more_than_one_official_match() {
        let pairs = [
            (
                git_source(
                    "Official Mirror A",
                    "https://github.com/fuyuxiang/echo-agent-desktop.git",
                ),
                entry("sentry"),
            ),
            (
                git_source(
                    "Official Mirror B",
                    "git@github.com:fuyuxiang/echo-agent-desktop.git",
                ),
                entry("sentry"),
            ),
        ];
        let scanned = scanned_entries(&pairs);
        assert_eq!(
            select_bare_name("sentry", &scanned),
            Err(BareNameError::Ambiguous {
                matched: vec![0, 1]
            })
        );
    }

    #[test]
    fn bare_name_not_found_when_no_entry_matches() {
        let pairs = [(
            git_source("src", "https://github.com/o/r.git"),
            entry("other"),
        )];
        let scanned = scanned_entries(&pairs);
        assert_eq!(
            select_bare_name("sentry", &scanned),
            Err(BareNameError::NotFound)
        );
    }
}
