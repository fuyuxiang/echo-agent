use gix::bstr::BString;
use gix::dir::entry::{Kind as DirEntryKind, Status as DirEntryStatus};
use gix::dir::walk::{CollapsedEntriesEmissionMode, EmissionMode, ForDeletionMode};
use gix::index::entry::Flags;
use gix::status::Item;
use gix::status::index_worktree::Item as IndexWorktreeItem;

use super::super::dirs::{has_any_file, pruned_path_holds_something};
use super::build_output::is_build_output;
use super::{Captured, KeepReason};

const HIDDEN_ENTRY_BITS: Flags = Flags::SKIP_WORKTREE.union(Flags::ASSUME_VALID);

pub(super) fn find_working_tree_content(
    repo: &gix::Repository,
    captured: Captured<'_>,
) -> Option<KeepReason> {
    let workdir = repo.workdir().unwrap_or_else(|| repo.git_dir()).to_owned();
    let index_has_entries = repo.index_path().metadata().map(|meta| meta.len() > 0);
    if !matches!(index_has_entries, Ok(true)) {
        tracing::warn!(
            git_dir = %repo.git_dir().display(),
            ?index_has_entries,
            "index is missing or empty"
        );
        return Some(KeepReason::CheckFailed);
    }
    let index = match repo.index() {
        Ok(index) => index,
        Err(error) => {
            tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to load index");
            return Some(KeepReason::CheckFailed);
        }
    };
    if index
        .entries()
        .iter()
        .any(|entry| entry.flags.intersects(HIDDEN_ENTRY_BITS))
    {
        return Some(KeepReason::HiddenFromStatus);
    }

    let platform = match repo.status(gix::progress::Discard) {
        Ok(platform) => platform,
        Err(error) => {
            tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to start status");
            return Some(KeepReason::CheckFailed);
        }
    };
    let walk = match repo.dirwalk_options() {
        Ok(options) => options,
        Err(error) => {
            tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to build dirwalk options");
            return Some(KeepReason::CheckFailed);
        }
    };
    let platform = platform.index_worktree_submodules(None);
    let platform = platform.index_worktree_options_mut(|options| {
        options.dirwalk_options = Some(
            walk.emit_untracked(EmissionMode::CollapseDirectory)
                .emit_ignored(Some(EmissionMode::CollapseDirectory))
                .emit_collapsed(Some(CollapsedEntriesEmissionMode::OnStatusMismatch))
                .emit_pruned(true)
                .for_deletion(Some(
                    ForDeletionMode::FindNonBareRepositoriesInIgnoredDirectories,
                )),
        );
    });
    let items = echo_agent_gix_status::with_budgeted_thread_limit(platform)
        .into_iter(Vec::<BString>::new());
    let items = match items {
        Ok(items) => items,
        Err(error) => {
            tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to iterate status");
            return Some(KeepReason::CheckFailed);
        }
    };
    let mut ignored_content = None;
    for item in items {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                tracing::warn!(git_dir = %repo.git_dir().display(), %error, "status item failed");
                return Some(KeepReason::CheckFailed);
            }
        };
        let dirty = match item {
            // Staged changes keep even under a snapshot: `add -A` captures the
            // working tree, never the index, so a staged-then-modified file
            // would otherwise lose its staged blob.
            Item::TreeIndex(_) => return Some(KeepReason::Dirty),
            Item::IndexWorktree(
                IndexWorktreeItem::Modification { .. } | IndexWorktreeItem::Rewrite { .. },
            ) => true,
            Item::IndexWorktree(IndexWorktreeItem::DirectoryContents { entry, .. }) => {
                match entry.status {
                    DirEntryStatus::Untracked | DirEntryStatus::Ignored(_)
                        if entry.disk_kind == Some(DirEntryKind::Repository) =>
                    {
                        return Some(KeepReason::EmbeddedRepo(entry.rela_path.to_string()));
                    }
                    DirEntryStatus::Untracked => !is_build_output(repo, &entry),
                    DirEntryStatus::Ignored(_) if is_build_output(repo, &entry) => false,
                    DirEntryStatus::Ignored(_) => {
                        ignored_content.get_or_insert_with(|| {
                            KeepReason::IgnoredContent(entry.rela_path.to_string())
                        });
                        false
                    }
                    DirEntryStatus::Pruned
                        if entry.rela_path != ".git"
                            && pruned_path_holds_something(
                                &workdir.join(gix::path::from_byte_slice(&entry.rela_path)),
                            ) =>
                    {
                        let holder = entry.rela_path.to_string();
                        let holder = holder.strip_suffix("/.git").unwrap_or(&holder);
                        return Some(KeepReason::EmbeddedRepo(holder.to_string()));
                    }
                    DirEntryStatus::Pruned | DirEntryStatus::Tracked => false,
                }
            }
        };
        if dirty && captured == Captured::Nothing {
            return Some(KeepReason::Dirty);
        }
    }
    ignored_content
}

pub(super) fn find_gitlink_content(repo: &gix::Repository) -> Option<KeepReason> {
    let workdir = repo.workdir()?;
    let index = match repo.index_or_empty() {
        Ok(index) => index,
        Err(error) => {
            tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to load index for submodule scan");
            return Some(KeepReason::CheckFailed);
        }
    };
    for entry in index.entries().iter().filter(|e| e.mode.is_submodule()) {
        let relative = gix::path::from_bstr(entry.path(&index)).into_owned();
        let path = workdir.join(&relative);
        match has_any_file(&path) {
            Ok(false) => {}
            Ok(true) => {
                tracing::info!(path = %path.display(), "a submodule checkout holds files");
                return Some(KeepReason::EmbeddedRepo(
                    relative.to_string_lossy().into_owned(),
                ));
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to read a submodule");
                return Some(KeepReason::CheckFailed);
            }
        }
    }
    None
}
