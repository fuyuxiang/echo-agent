use std::io::Read;
use std::path::Path;

use gix::dir::entry::Kind as DirEntryKind;
use gix::index::entry::Mode;

const TOOL_OUTPUT_DIRECTORIES: &[&str] = &[
    "node_modules",
    ".venv",
    ".next",
    "__pycache__",
    ".ruff_cache",
    ".pytest_cache",
    ".mypy_cache",
    ".pnpm-store",
    "target",
    "dist",
    "build",
];

const BUILD_OUTPUT_SYMLINK_PREFIX: &str = "bazel-";

const CACHE_TAG: &str = "CACHEDIR.TAG";
// The cachedir.org tag file's fixed first-line signature.
const CACHE_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Whether an ignored/untracked entry is a tool's output (safe to drop) rather
/// than the user's work. A known name alone is not enough: the directory must be
/// written off by the repository's *own* ignore rules (a user's global/system
/// gitignore must not authorize deletion) or carry a `CACHEDIR.TAG`.
pub(super) fn is_build_output(repo: &gix::Repository, entry: &gix::dir::Entry) -> bool {
    let Some(workdir) = repo.workdir() else {
        return false;
    };
    let relative_path = entry.rela_path.to_string();
    let Some(name) = relative_path.rsplit('/').find(|name| !name.is_empty()) else {
        return false;
    };
    match entry.disk_kind {
        Some(DirEntryKind::Symlink) => {
            name.starts_with(BUILD_OUTPUT_SYMLINK_PREFIX)
                && excluded_by_its_own_rule(repo, &relative_path, Mode::SYMLINK)
        }
        Some(kind) if kind.is_dir() => {
            (TOOL_OUTPUT_DIRECTORIES.contains(&name)
                && excluded_by_its_own_rule(repo, &relative_path, Mode::DIR))
                || under_cache_tag(workdir, &relative_path)
        }
        _ => false,
    }
}

fn excluded_by_its_own_rule(repo: &gix::Repository, relative_path: &str, mode: Mode) -> bool {
    let Ok(index) = repo.index_or_empty() else {
        return false;
    };
    let Ok(mut excludes) = repo.excludes(
        &index,
        None,
        gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
    ) else {
        return false;
    };
    let Ok(platform) = excludes.at_entry(relative_path, Some(mode)) else {
        return false;
    };
    let Some(matched) = platform.matching_exclude_pattern() else {
        return false;
    };
    if matched.pattern.is_negative() {
        return false;
    }
    matched
        .source
        .is_some_and(|source| is_repository_own_ignore_source(source, repo))
}

fn is_repository_own_ignore_source(source: &Path, repo: &gix::Repository) -> bool {
    if source.file_name().is_some_and(|name| name == "exclude")
        && source
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == "info")
    {
        let Some(git_base) = source.parent().and_then(|parent| parent.parent()) else {
            return false;
        };
        return git_base == repo.common_dir() || git_base == repo.git_dir();
    }
    if source.file_name().is_some_and(|name| name == ".gitignore") {
        return repo
            .workdir()
            .is_some_and(|workdir| source.starts_with(workdir));
    }
    false
}

fn under_cache_tag(workdir: &Path, relative_path: &str) -> bool {
    workdir
        .join(relative_path)
        .ancestors()
        .take_while(|directory| *directory != workdir && directory.starts_with(workdir))
        .any(carries_cache_tag)
}

fn carries_cache_tag(directory: &Path) -> bool {
    let tag = directory.join(CACHE_TAG);
    if !std::fs::symlink_metadata(&tag).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(&tag) else {
        return false;
    };
    let mut signature = Vec::with_capacity(CACHE_TAG_SIGNATURE.len());
    let want = u64::try_from(CACHE_TAG_SIGNATURE.len()).unwrap_or(u64::MAX);
    file.take(want).read_to_end(&mut signature).is_ok() && signature == CACHE_TAG_SIGNATURE
}
