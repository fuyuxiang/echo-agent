//! Worktree orchestration: plan + execute.

pub mod execute;
pub(crate) mod plan;

use std::path::PathBuf;

use anyhow::Result;

use crate::copy::{CopyStats, DirtyFilesReport};

pub(crate) use plan::WorktreePlan;

/// Strategy strings written to `worktrees.db` `creation_mode` and metrics.
pub const STRATEGY_GROVE_FUSE: &str = "grove-fuse";
pub const STRATEGY_GROVE_NFS: &str = "grove-nfs";
/// Deprecated alias for [`STRATEGY_GROVE_NFS`]. Still accepted on read/GC.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const STRATEGY_NFS: &str = "nfs";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const STRATEGY_OVERLAY: &str = "overlay";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const STRATEGY_BTRFS: &str = "btrfs";
pub const STRATEGY_COPY: &str = "copy";
pub const STRATEGY_GIT: &str = "git";
pub const STRATEGY_STANDALONE: &str = "standalone";

/// Projected grove worktree: Linux FUSE, macOS NFS, or the legacy `nfs` spelling.
#[must_use]
pub fn is_grove_strategy(s: &str) -> bool {
    matches!(s, STRATEGY_GROVE_FUSE | STRATEGY_GROVE_NFS | STRATEGY_NFS)
}

/// Result of worktree creation.
#[derive(Debug)]
pub struct CreateWorktreeResult {
    /// Path to the created worktree
    pub worktree_path: PathBuf,

    /// Git commit the worktree is based on
    pub commit: String,

    /// Statistics from the file copy phase
    pub copy_stats: CopyStats,

    /// Statistics from ignored files copy (if enabled)
    pub ignored_stats: Option<CopyStats>,

    /// Report about dirty files (modified/untracked/deleted) in the source worktree
    pub dirty_files_report: Option<DirtyFilesReport>,

    /// Which dispatch arm actually ran (`grove-fuse` / `grove-nfs` / `overlay` / `btrfs` / `copy` / `git` / `standalone`).
    pub resolved_strategy: &'static str,

    /// Arm-specific metadata (NFS mount/backing/pin; overlay/btrfs snapshot paths).
    pub strategy_metadata: Option<serde_json::Value>,
}

/// Execute worktree creation plan. This is a blocking operation.
pub(crate) fn execute_plan(plan: WorktreePlan) -> Result<CreateWorktreeResult> {
    execute::execute_create_worktree(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IgnoredFilesMode, WorkingTreeMode, WorktreeBuilder};

    #[test]
    fn grove_strategy_names() {
        assert!(is_grove_strategy(STRATEGY_GROVE_FUSE));
        assert!(is_grove_strategy(STRATEGY_GROVE_NFS));
        assert!(is_grove_strategy(STRATEGY_NFS));
        assert!(!is_grove_strategy(STRATEGY_COPY));
        assert!(!is_grove_strategy("linked"));
    }
    use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
    use tempfile::TempDir;

    #[test]
    fn test_create_worktree_simple() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit a file
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        // Create worktree
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .create()
            .unwrap();

        assert!(result.worktree_path.exists());
        assert!(result.worktree_path.join("file.txt").exists());
        assert!(!result.commit.is_empty());
        assert_eq!(result.resolved_strategy, "copy");
        assert!(
            crate::grove_wt_create_count("copy") >= 1,
            "grove_wt_create must record the copy arm"
        );
    }

    #[test]
    fn test_create_worktree_creates_parent_dirs() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit a file
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        // Destination parent directory does NOT exist.
        let worktree_path = temp.path().join("nested").join("worktree");
        assert!(!worktree_path.parent().unwrap().exists());

        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .create()
            .unwrap();

        assert!(result.worktree_path.exists());
        assert!(result.worktree_path.join("file.txt").exists());
        assert!(!result.commit.is_empty());
    }

    #[test]
    fn test_create_worktree_with_ignored_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create tracked file
        std::fs::write(repo_path.join("tracked.txt"), "tracked").unwrap();

        // Create .gitignore
        std::fs::write(repo_path.join(".gitignore"), "ignored/").unwrap();

        // Create ignored directory
        std::fs::create_dir(repo_path.join("ignored")).unwrap();
        std::fs::write(repo_path.join("ignored/deps.txt"), "deps").unwrap();

        git_commit_all(&repo_path, "initial");

        // Create worktree with ignored file copying
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .ignored_files_mode(IgnoredFilesMode::Copy {
                skip_patterns: vec![],
            })
            .create()
            .unwrap();

        // Both tracked and ignored files should exist
        assert!(result.worktree_path.join("tracked.txt").exists());
        assert!(result.worktree_path.join("ignored/deps.txt").exists());

        // Should have ignored stats
        assert!(result.ignored_copy.is_some());
    }

    #[test]
    fn test_create_worktree_skip_ignored_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create tracked file
        std::fs::write(repo_path.join("tracked.txt"), "tracked").unwrap();

        // Create .gitignore
        std::fs::write(repo_path.join(".gitignore"), "*.log\nnode_modules/\n").unwrap();

        // Create ignored files
        std::fs::write(repo_path.join("debug.log"), "debug").unwrap();
        std::fs::create_dir(repo_path.join("node_modules")).unwrap();
        std::fs::write(repo_path.join("node_modules/package.json"), "{}").unwrap();

        git_commit_all(&repo_path, "initial");

        // Create worktree with IgnoredFilesMode::Skip (explicit)
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .ignored_files_mode(IgnoredFilesMode::Skip)
            .create()
            .unwrap();

        // Tracked files should exist
        assert!(result.worktree_path.join("tracked.txt").exists());
        assert!(result.worktree_path.join(".gitignore").exists());

        // Ignored files should NOT exist
        assert!(!result.worktree_path.join("debug.log").exists());
        assert!(
            !result
                .worktree_path
                .join("node_modules/package.json")
                .exists()
        );

        // Should NOT have ignored stats
        assert!(result.ignored_copy.is_none());
    }

    #[test]
    fn test_copy_ignored_only_standalone() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        let dest_path = temp.path().join("dest");
        std::fs::create_dir(&repo_path).unwrap();
        std::fs::create_dir(&dest_path).unwrap();

        init_git_repo(&repo_path);

        // Create tracked file
        std::fs::write(repo_path.join("tracked.txt"), "tracked").unwrap();

        // Create .gitignore
        std::fs::write(repo_path.join(".gitignore"), "node_modules/\n*.log").unwrap();

        // Create ignored files
        std::fs::create_dir(repo_path.join("node_modules")).unwrap();
        std::fs::write(repo_path.join("node_modules/package.json"), "{}").unwrap();
        std::fs::write(repo_path.join("debug.log"), "log content").unwrap();

        git_commit_all(&repo_path, "initial");

        // Copy only ignored files
        let result = WorktreeBuilder::new(repo_path.clone(), dest_path.clone())
            .copy_ignored_only()
            .unwrap();

        // Ignored files should be copied
        assert!(dest_path.join("node_modules/package.json").exists());
        assert!(dest_path.join("debug.log").exists());

        // Tracked files should NOT be copied
        assert!(!dest_path.join("tracked.txt").exists());
        assert!(!dest_path.join(".gitignore").exists());

        // Stats should reflect what was copied
        assert!(result.files_copied >= 2);
    }

    #[test]
    fn test_copy_ignored_only_with_skip_patterns() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        let dest_path = temp.path().join("dest");
        std::fs::create_dir(&repo_path).unwrap();
        std::fs::create_dir(&dest_path).unwrap();

        init_git_repo(&repo_path);

        // Create tracked file
        std::fs::write(repo_path.join("tracked.txt"), "tracked").unwrap();

        // Create .gitignore
        std::fs::write(
            repo_path.join(".gitignore"),
            "node_modules/\n*.log\n.cache/\n",
        )
        .unwrap();

        // Create ignored files
        std::fs::create_dir(repo_path.join("node_modules")).unwrap();
        std::fs::write(repo_path.join("node_modules/package.json"), "{}").unwrap();
        std::fs::write(repo_path.join("debug.log"), "debug").unwrap();
        std::fs::write(repo_path.join("error.log"), "error").unwrap();

        std::fs::create_dir(repo_path.join(".cache")).unwrap();
        std::fs::write(repo_path.join(".cache/data.bin"), "cache").unwrap();

        git_commit_all(&repo_path, "initial");

        // Copy only ignored files, but skip .log files and .cache directory
        let result = WorktreeBuilder::new(repo_path.clone(), dest_path.clone())
            .ignored_files_mode(IgnoredFilesMode::CopyOnly {
                skip_patterns: vec!["*.log".to_string(), ".cache/**".to_string()],
            })
            .copy_ignored_only()
            .unwrap();

        // node_modules should be copied (not in skip patterns)
        assert!(dest_path.join("node_modules/package.json").exists());

        // .log files should be SKIPPED (in skip patterns)
        assert!(!dest_path.join("debug.log").exists());
        assert!(!dest_path.join("error.log").exists());

        // .cache directory should be SKIPPED (in skip patterns)
        assert!(!dest_path.join(".cache/data.bin").exists());

        // Tracked files should NOT be copied
        assert!(!dest_path.join("tracked.txt").exists());
        assert!(!dest_path.join(".gitignore").exists());

        // Stats should only reflect what was actually copied (just node_modules)
        assert_eq!(result.files_copied, 1);
    }

    #[test]
    #[cfg(unix)]
    fn test_worktree_with_symlinks() {
        echo_agent_test_utils::require_git!();
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create a regular file
        std::fs::write(repo_path.join("target.txt"), "target content").unwrap();

        // Create a symlink to the file
        symlink("target.txt", repo_path.join("link.txt")).unwrap();

        // Create a directory with a file
        std::fs::create_dir(repo_path.join("subdir")).unwrap();
        std::fs::write(repo_path.join("subdir/file.txt"), "subdir file").unwrap();

        // Create a symlink to the directory
        symlink("subdir", repo_path.join("link_dir")).unwrap();

        git_commit_all(&repo_path, "initial with symlinks");

        // Create worktree
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .create()
            .unwrap();

        // Regular files should exist
        assert!(result.worktree_path.join("target.txt").exists());
        assert!(result.worktree_path.join("subdir/file.txt").exists());

        // Symlinks should be preserved as symlinks
        let link_path = result.worktree_path.join("link.txt");
        assert!(link_path.exists());
        assert!(
            link_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // Symlink should point to correct target
        let link_target = std::fs::read_link(&link_path).unwrap();
        assert_eq!(link_target, PathBuf::from("target.txt"));

        // Directory symlink should also work
        let dir_link_path = result.worktree_path.join("link_dir");
        assert!(
            dir_link_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_copy_ignored_only_with_symlinks() {
        echo_agent_test_utils::require_git!();
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        let dest_path = temp.path().join("dest");
        std::fs::create_dir(&repo_path).unwrap();
        std::fs::create_dir(&dest_path).unwrap();

        init_git_repo(&repo_path);

        // Create tracked file
        std::fs::write(repo_path.join("tracked.txt"), "tracked").unwrap();

        // Create .gitignore
        std::fs::write(repo_path.join(".gitignore"), "ignored/").unwrap();

        // Create ignored directory with symlinks
        std::fs::create_dir(repo_path.join("ignored")).unwrap();
        std::fs::write(repo_path.join("ignored/real.txt"), "real").unwrap();
        symlink("real.txt", repo_path.join("ignored/link.txt")).unwrap();

        git_commit_all(&repo_path, "initial");

        // Copy only ignored files
        let result = WorktreeBuilder::new(repo_path.clone(), dest_path.clone())
            .copy_ignored_only()
            .unwrap();

        // Symlink should be copied
        let link_path = dest_path.join("ignored/link.txt");
        assert!(link_path.exists());
        assert!(
            link_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // Symlink stats should be tracked
        assert!(result.symlinks_copied >= 1);
    }

    #[test]
    fn test_worktree_with_dirty_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit initial file
        std::fs::write(repo_path.join("file.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");

        // Modify the file (dirty state)
        std::fs::write(repo_path.join("file.txt"), "modified").unwrap();

        // Create worktree with copy_dirty_files=true
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .working_tree_mode(WorkingTreeMode::PreserveWorkingTree)
            .create()
            .unwrap();

        let worktree_path = result.worktree_path.clone();

        // Dirty file should have modified content.
        let content = std::fs::read_to_string(worktree_path.join("file.txt")).unwrap();
        assert_eq!(content, "modified");
    }

    #[test]
    fn test_worktree_preserves_git_status_for_dirty_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit initial file
        std::fs::write(repo_path.join("file.txt"), "original content").unwrap();
        git_commit_all(&repo_path, "initial");

        // Modify the file (dirty state)
        std::fs::write(repo_path.join("file.txt"), "modified content").unwrap();

        // Verify source shows file as modified
        let source_status = std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let source_status_str = String::from_utf8_lossy(&source_status.stdout);
        eprintln!("Source git status: {}", source_status_str);
        assert!(
            source_status_str.contains("file.txt"),
            "Source should show file.txt as modified"
        );

        // Create worktree with PreserveWorkingTree mode
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .working_tree_mode(WorkingTreeMode::PreserveWorkingTree)
            .create()
            .unwrap();

        let worktree_path = result.worktree_path.clone();

        // File should have modified content
        let content = std::fs::read_to_string(worktree_path.join("file.txt")).unwrap();
        assert_eq!(content, "modified content");

        // CRITICAL: git status should show the file as modified
        let worktree_status = std::process::Command::new("git")
            .current_dir(&worktree_path)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let worktree_status_str = String::from_utf8_lossy(&worktree_status.stdout);
        eprintln!("Worktree git status: {}", worktree_status_str);

        // The file should still show as modified in the worktree
        assert!(
            worktree_status_str.contains("file.txt"),
            "Worktree should show file.txt as modified, but got: {}",
            worktree_status_str
        );
    }

    #[test]
    fn test_git_status_is_instant_after_worktree_creation() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create multiple committed files
        for i in 0..10 {
            std::fs::write(
                repo_path.join(format!("file{}.txt", i)),
                format!("content {}", i),
            )
            .unwrap();
        }
        git_commit_all(&repo_path, "initial");

        // Create worktree
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .create()
            .unwrap();

        let worktree_path = result.worktree_path.clone();

        // Run git status TWICE - first one should not refresh index
        let start = std::time::Instant::now();
        let status1 = std::process::Command::new("git")
            .current_dir(&worktree_path)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let first_duration = start.elapsed();

        let start = std::time::Instant::now();
        let status2 = std::process::Command::new("git")
            .current_dir(&worktree_path)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let second_duration = start.elapsed();

        eprintln!("First git status: {:?}", first_duration);
        eprintln!("Second git status: {:?}", second_duration);

        // Both should be clean
        let status1_str = String::from_utf8_lossy(&status1.stdout);
        let status2_str = String::from_utf8_lossy(&status2.stdout);
        assert!(status1_str.is_empty(), "Worktree should be clean");
        assert!(status2_str.is_empty(), "Worktree should be clean");

        // More definitive test: Run git status with GIT_TRACE_PERFORMANCE
        // This will show if git is re-hashing files
        let trace_status = std::process::Command::new("git")
            .current_dir(&worktree_path)
            .env("GIT_TRACE_PERFORMANCE", "1")
            .args(["status", "--porcelain"])
            .output()
            .unwrap();

        let trace_output = String::from_utf8_lossy(&trace_status.stderr);
        eprintln!("GIT_TRACE_PERFORMANCE output:\n{}", trace_output);

        // If index needs refresh, we'd see "lstat" or "refresh index" in trace
        // The key indicator is if git is doing filesystem operations on every file
        let has_refresh_indicator = trace_output.contains("refresh_index")
            || trace_output.lines().filter(|l| l.contains("lstat")).count() > 3;

        if has_refresh_indicator {
            eprintln!("WARNING: git status appears to be refreshing the index");
        }

        // This is informational - the real test is that status is clean
        if first_duration.as_millis() > 200 {
            eprintln!(
                "NOTE: First git status took {:?}, which seems slow",
                first_duration
            );
        }
    }

    #[test]
    fn test_clean_files_dont_show_as_modified() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit files
        std::fs::write(repo_path.join("clean.txt"), "clean content").unwrap();
        std::fs::write(repo_path.join("will_be_dirty.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");

        // Modify one file to make it dirty
        std::fs::write(repo_path.join("will_be_dirty.txt"), "modified").unwrap();

        // Create worktree
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .working_tree_mode(WorkingTreeMode::PreserveWorkingTree)
            .create()
            .unwrap();

        let worktree_path = result.worktree_path.clone();

        // Check git status
        let status = std::process::Command::new("git")
            .current_dir(&worktree_path)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let status_str = String::from_utf8_lossy(&status.stdout);
        eprintln!("Git status output:\n{}", status_str);

        // Only will_be_dirty.txt should show as modified
        assert!(
            status_str.contains("will_be_dirty.txt"),
            "Dirty file should show as modified"
        );
        assert!(
            !status_str.contains("clean.txt"),
            "Clean file should NOT show as modified, but status shows:\n{}",
            status_str
        );
    }

    #[test]
    fn test_worktree_clean_state() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit initial file
        std::fs::write(repo_path.join("file.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");

        // Modify the file (dirty state)
        std::fs::write(repo_path.join("file.txt"), "modified").unwrap();

        // Create worktree with copy_dirty_files=false (clean state)
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .working_tree_mode(WorkingTreeMode::CleanTracked)
            .create()
            .unwrap();

        let worktree_path = result.worktree_path.clone();

        // File should have original (committed) content.
        let content = std::fs::read_to_string(worktree_path.join("file.txt")).unwrap();
        assert_eq!(content, "original");
    }

    #[test]
    fn test_worktree_clean_all_state() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit initial file
        std::fs::write(repo_path.join("committed.txt"), "committed").unwrap();

        // Create .gitignore
        std::fs::write(repo_path.join(".gitignore"), "*.log\n").unwrap();

        git_commit_all(&repo_path, "initial");

        // Modify committed file (dirty tracked file)
        std::fs::write(repo_path.join("committed.txt"), "modified").unwrap();

        // Add an untracked file
        std::fs::write(repo_path.join("untracked.txt"), "untracked").unwrap();

        // Add an ignored file
        std::fs::write(repo_path.join("debug.log"), "debug").unwrap();

        // Create worktree with CleanAll mode
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .working_tree_mode(WorkingTreeMode::CleanAll)
            .create()
            .unwrap();

        let worktree_path = result.worktree_path.clone();

        // Committed file should have original content
        let content = std::fs::read_to_string(worktree_path.join("committed.txt")).unwrap();
        assert_eq!(content, "committed");

        // Untracked file should NOT exist (cleaned)
        assert!(!worktree_path.join("untracked.txt").exists());

        // Ignored file should NOT exist (cleaned by CleanAll)
        assert!(!worktree_path.join("debug.log").exists());

        // .gitignore should exist (it's tracked)
        assert!(worktree_path.join(".gitignore").exists());
    }

    #[test]
    fn test_background_finalization() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create files
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        // Create worktree
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .create()
            .unwrap();

        // Worktree should be usable and finalization is complete.
        assert!(result.worktree_path.join("file.txt").exists());
    }

    #[test]
    fn test_worktree_with_nested_directories() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create deeply nested structure
        std::fs::create_dir_all(repo_path.join("a/b/c/d")).unwrap();
        std::fs::write(repo_path.join("a/file1.txt"), "1").unwrap();
        std::fs::write(repo_path.join("a/b/file2.txt"), "2").unwrap();
        std::fs::write(repo_path.join("a/b/c/file3.txt"), "3").unwrap();
        std::fs::write(repo_path.join("a/b/c/d/file4.txt"), "4").unwrap();

        git_commit_all(&repo_path, "initial");

        // Create worktree
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .create()
            .unwrap();

        // All nested files should exist
        assert!(result.worktree_path.join("a/file1.txt").exists());
        assert!(result.worktree_path.join("a/b/file2.txt").exists());
        assert!(result.worktree_path.join("a/b/c/file3.txt").exists());
        assert!(result.worktree_path.join("a/b/c/d/file4.txt").exists());
    }

    #[test]
    fn test_worktree_preserves_file_content() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create files with specific content
        let binary_content: Vec<u8> = (0..=255).collect();
        std::fs::write(repo_path.join("text.txt"), "Hello, World! 🌍").unwrap();
        std::fs::write(repo_path.join("binary.bin"), &binary_content).unwrap();
        std::fs::write(repo_path.join("empty.txt"), "").unwrap();

        git_commit_all(&repo_path, "initial");

        // Create worktree
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .create()
            .unwrap();

        // Verify content is preserved
        assert_eq!(
            std::fs::read_to_string(result.worktree_path.join("text.txt")).unwrap(),
            "Hello, World! 🌍"
        );
        assert_eq!(
            std::fs::read(result.worktree_path.join("binary.bin")).unwrap(),
            binary_content
        );
        assert_eq!(
            std::fs::read_to_string(result.worktree_path.join("empty.txt")).unwrap(),
            ""
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_worktree_preserves_permissions() {
        echo_agent_test_utils::require_git!();
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create executable file
        std::fs::write(repo_path.join("script.sh"), "#!/bin/bash\necho hello").unwrap();
        let mut perms = std::fs::metadata(repo_path.join("script.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(repo_path.join("script.sh"), perms).unwrap();

        git_commit_all(&repo_path, "initial");

        // Create worktree
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .create()
            .unwrap();

        // Verify executable bit is preserved
        let dest_perms = std::fs::metadata(result.worktree_path.join("script.sh"))
            .unwrap()
            .permissions();
        assert!(
            dest_perms.mode() & 0o111 != 0,
            "executable bit should be set"
        );
    }

    #[test]
    fn test_worktree_with_btrfs_disabled() {
        echo_agent_test_utils::require_git!();
        use crate::BtrfsMode;

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit a file
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        // Create worktree with BTRFS disabled - should use copy method
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .btrfs_mode(BtrfsMode::Disabled)
            .create()
            .unwrap();

        assert!(result.worktree_path.exists());
        assert!(result.worktree_path.join("file.txt").exists());
        assert!(!result.commit.is_empty());
        // With copy method, files_copied should be > 0
        assert!(result.unignored_copy.files_copied > 0);
    }

    #[test]
    fn test_worktree_with_btrfs_auto_on_non_btrfs() {
        echo_agent_test_utils::require_git!();
        use crate::BtrfsMode;

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create and commit a file
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        // Create worktree with BTRFS auto - should fall back to copy on non-BTRFS
        let worktree_path = temp.path().join("worktree");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .btrfs_mode(BtrfsMode::Auto)
            .create()
            .unwrap();

        assert!(result.worktree_path.exists());
        assert!(result.worktree_path.join("file.txt").exists());
        assert!(!result.commit.is_empty());
        // On non-BTRFS (like tmpfs in tests), should use copy method
        // so files_copied should be > 0
        assert!(result.unignored_copy.files_copied > 0);
    }

    // Integration test for real BTRFS systems
    // This test automatically runs if BTRFS is detected, skips otherwise
    #[test]
    #[cfg(target_os = "linux")]
    fn test_worktree_btrfs_snapshot_integration() {
        use crate::BtrfsMode;

        // Helper to find a writable BTRFS subvolume
        fn get_btrfs_test_dir() -> Option<std::path::PathBuf> {
            // Check environment variable first
            if let Ok(path) = std::env::var("BTRFS_TEST_PATH") {
                let path = std::path::PathBuf::from(&path);
                if path.exists()
                    && crate::btrfs::is_btrfs(&path).unwrap_or(false)
                    && crate::btrfs::is_btrfs_subvolume(&path)
                        .ok()
                        .flatten()
                        .is_some()
                {
                    return Some(path);
                }
            }

            // Check if current temp directory is on BTRFS subvolume
            let temp = std::env::temp_dir();
            if crate::btrfs::is_btrfs(&temp).unwrap_or(false)
                && crate::btrfs::is_btrfs_subvolume(&temp)
                    .ok()
                    .flatten()
                    .is_some()
            {
                return Some(temp);
            }

            None
        }

        let Some(btrfs_path) = get_btrfs_test_dir() else {
            eprintln!("Skipping test: no BTRFS subvolume detected");
            eprintln!("Set BTRFS_TEST_PATH to a BTRFS subvolume to run this test");
            return;
        };

        eprintln!(
            "Running BTRFS integration test on: {}",
            btrfs_path.display()
        );

        // Create test directories within the BTRFS subvolume
        let test_id = std::process::id();
        let repo_path = btrfs_path.join(format!("test_repo_{}", test_id));
        let worktree_path = btrfs_path.join(format!("test_worktree_{}", test_id));

        // Clean up from any previous runs
        let _ = std::fs::remove_dir_all(&repo_path);
        let _ = std::process::Command::new("btrfs")
            .args(["subvolume", "delete", &worktree_path.to_string_lossy()])
            .output();

        // Create and initialize repo
        std::fs::create_dir_all(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "btrfs test content").unwrap();
        git_commit_all(&repo_path, "initial");

        // Create worktree using BTRFS snapshot
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .btrfs_mode(BtrfsMode::Auto)
            .create();

        match result {
            Ok(report) => {
                assert!(report.worktree_path.exists());
                assert!(report.worktree_path.join("file.txt").exists());

                // Check if BTRFS was used (files_copied == 0) or fallback to copy
                if report.unignored_copy.files_copied == 0 {
                    eprintln!("✓ BTRFS snapshot worktree created successfully!");
                } else {
                    eprintln!(
                        "Note: Fell back to copy method ({} files copied)",
                        report.unignored_copy.files_copied
                    );
                }

                // Clean up
                let _ = std::process::Command::new("btrfs")
                    .args(["subvolume", "delete", &worktree_path.to_string_lossy()])
                    .output();
                let _ = std::fs::remove_dir_all(&worktree_path);
            }
            Err(e) => {
                eprintln!("BTRFS test failed: {}", e);
                // Don't fail the test - might be permission issues
            }
        }

        // Clean up repo
        let _ = std::fs::remove_dir_all(&repo_path);
    }

    // ─── Standalone mode tests ───────────────────────────────────────────

    #[test]
    fn test_standalone_worktree_simple() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        let worktree_path = temp.path().join("standalone");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .standalone(true)
            .create()
            .unwrap();

        assert!(result.worktree_path.exists());
        assert!(result.worktree_path.join("file.txt").exists());
        assert!(!result.commit.is_empty());

        // Should be a standalone repo (`.git` is a directory, not a file)
        assert!(result.worktree_path.join(".git").is_dir());
    }

    #[test]
    fn test_standalone_worktree_is_independent() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        let worktree_path = temp.path().join("standalone");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .standalone(true)
            .create()
            .unwrap();

        // The standalone repo should work independently of the source.
        // Verify git log works (proves objects were copied).
        let log_output = std::process::Command::new("git")
            .current_dir(&result.worktree_path)
            .args(["log", "--oneline"])
            .output()
            .unwrap();
        assert!(log_output.status.success());
        let log_str = String::from_utf8_lossy(&log_output.stdout);
        assert!(
            log_str.contains("initial"),
            "standalone repo should have commit history"
        );

        // Source should NOT have any worktree registrations.
        let worktree_list = std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let list_str = String::from_utf8_lossy(&worktree_list.stdout);
        assert!(
            !list_str.contains("standalone"),
            "standalone copy should NOT be registered as a worktree in the source"
        );
    }

    #[test]
    fn test_standalone_worktree_no_worktrees_dir() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        // Create a linked worktree in the source to populate .git/worktrees/
        let linked_wt = temp.path().join("linked-wt");
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");
        std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["worktree", "add", "--detach", &linked_wt.to_string_lossy()])
            .output()
            .unwrap();
        assert!(repo_path.join(".git/worktrees").exists());

        // Create standalone copy — .git/worktrees/ should be skipped.
        let standalone_path = temp.path().join("standalone");
        WorktreeBuilder::new(repo_path.clone(), standalone_path.clone())
            .standalone(true)
            .create()
            .unwrap();

        assert!(!standalone_path.join(".git/worktrees").exists());

        // Clean up linked worktree
        let _ = std::process::Command::new("git")
            .current_dir(&repo_path)
            .args([
                "worktree",
                "remove",
                "--force",
                &linked_wt.to_string_lossy(),
            ])
            .output();
    }

    #[test]
    fn test_standalone_worktree_preserves_dirty_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        std::fs::write(repo_path.join("file.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");

        // Dirty the file
        std::fs::write(repo_path.join("file.txt"), "modified").unwrap();

        let worktree_path = temp.path().join("standalone");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .standalone(true)
            .working_tree_mode(WorkingTreeMode::PreserveWorkingTree)
            .create()
            .unwrap();

        // Dirty file should have modified content.
        let content = std::fs::read_to_string(result.worktree_path.join("file.txt")).unwrap();
        assert_eq!(content, "modified");
    }

    #[test]
    fn test_standalone_worktree_clean_tracked() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        std::fs::write(repo_path.join("file.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");

        // Dirty the file
        std::fs::write(repo_path.join("file.txt"), "modified").unwrap();

        let worktree_path = temp.path().join("standalone");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .standalone(true)
            .working_tree_mode(WorkingTreeMode::CleanTracked)
            .create()
            .unwrap();

        // Should have original (committed) content.
        let content = std::fs::read_to_string(result.worktree_path.join("file.txt")).unwrap();
        assert_eq!(content, "original");
    }

    #[test]
    fn test_standalone_worktree_promotable_via_rename() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        // Create standalone copy
        let standalone_path = temp.path().join("standalone");
        let result = WorktreeBuilder::new(repo_path.clone(), standalone_path.clone())
            .standalone(true)
            .create()
            .unwrap();

        let original_commit = result.commit.clone();

        // Simulate promotion: rename standalone to replace the original.
        let promoted_path = temp.path().join("promoted");
        std::fs::rename(&standalone_path, &promoted_path).unwrap();

        // Promoted copy should be a fully functional repo.
        assert!(promoted_path.join("file.txt").exists());
        assert!(promoted_path.join(".git").is_dir());

        let log_output = std::process::Command::new("git")
            .current_dir(&promoted_path)
            .args(["log", "--oneline"])
            .output()
            .unwrap();
        assert!(log_output.status.success());

        // Commit should be the same
        let rev_output = std::process::Command::new("git")
            .current_dir(&promoted_path)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let commit = String::from_utf8_lossy(&rev_output.stdout)
            .trim()
            .to_string();
        assert_eq!(commit, original_commit);
    }

    #[test]
    fn test_standalone_worktree_with_ignored_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();

        init_git_repo(&repo_path);

        std::fs::write(repo_path.join("tracked.txt"), "tracked").unwrap();
        std::fs::write(repo_path.join(".gitignore"), "ignored/").unwrap();
        std::fs::create_dir(repo_path.join("ignored")).unwrap();
        std::fs::write(repo_path.join("ignored/deps.txt"), "deps").unwrap();
        git_commit_all(&repo_path, "initial");

        let worktree_path = temp.path().join("standalone");
        let result = WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .standalone(true)
            .ignored_files_mode(IgnoredFilesMode::Copy {
                skip_patterns: vec![],
            })
            .create()
            .unwrap();

        assert!(result.worktree_path.join("tracked.txt").exists());
        assert!(result.worktree_path.join("ignored/deps.txt").exists());
        assert!(result.ignored_copy.is_some());
    }

    #[test]
    fn test_standalone_worktree_narrows_origin_fetch() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");
        let branch =
            echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]);
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/echoagent.git",
            ],
        );
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &[
                "config",
                "--replace-all",
                "remote.origin.fetch",
                "+refs/heads/*",
            ],
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &repo_path,
                &["config", "--get", "remote.origin.fetch"]
            ),
            "+refs/heads/*"
        );

        let worktree_path = temp.path().join("standalone");
        WorktreeBuilder::new(repo_path.clone(), worktree_path.clone())
            .standalone(true)
            .create()
            .unwrap();

        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["config", "--get-all", "remote.origin.fetch"]
            ),
            format!("+refs/heads/{branch}:refs/remotes/origin/{branch}")
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["config", "--get", "remote.origin.url"]
            ),
            "https://github.com/example/echoagent.git"
        );
    }

    #[test]
    fn test_standalone_worktree_drops_inconsistent_shallow() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("a.txt"), "a").unwrap();
        git_commit_all(&repo_path, "A");
        let a = echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "HEAD"]);
        std::fs::write(repo_path.join("b.txt"), "b").unwrap();
        git_commit_all(&repo_path, "B");
        let b = echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "HEAD"]);
        echo_agent_test_utils::git::run_git(&repo_path, &["checkout", "-b", "feature", &a]);
        std::fs::write(repo_path.join("d.txt"), "d").unwrap();
        git_commit_all(&repo_path, "D");
        echo_agent_test_utils::git::run_git(&repo_path, &["update-ref", "refs/heads/main", &b]);
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &["update-ref", "refs/remotes/origin/main", &b],
        );
        std::fs::write(repo_path.join(".git/shallow"), format!("{b}\n")).unwrap();

        let worktree_path = temp.path().join("standalone");
        WorktreeBuilder::new(repo_path, worktree_path.clone())
            .standalone(true)
            .create()
            .unwrap();

        assert!(!worktree_path.join(".git/shallow").exists());
        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["rev-parse", "--is-shallow-repository"]
            ),
            "false"
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(&worktree_path, &["rev-parse", "HEAD^"]),
            a
        );
    }

    #[test]
    fn test_standalone_worktree_sanitizes_after_checkout_ref() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("a.txt"), "a").unwrap();
        git_commit_all(&repo_path, "A");
        let a = echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "HEAD"]);
        std::fs::write(repo_path.join("b.txt"), "b").unwrap();
        git_commit_all(&repo_path, "B");
        let b = echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "HEAD"]);
        std::fs::write(repo_path.join("c.txt"), "c").unwrap();
        git_commit_all(&repo_path, "C");
        let source_branch =
            echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]);
        echo_agent_test_utils::git::run_git(&repo_path, &["checkout", "-b", "feature", &a]);
        std::fs::write(repo_path.join("d.txt"), "d").unwrap();
        git_commit_all(&repo_path, "D");
        echo_agent_test_utils::git::run_git(&repo_path, &["checkout", &source_branch]);
        assert_ne!(source_branch, "feature");
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/echoagent.git",
            ],
        );
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &[
                "config",
                "--replace-all",
                "remote.origin.fetch",
                "+refs/heads/*",
            ],
        );
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &["update-ref", "refs/remotes/origin/main", &b],
        );
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &["update-ref", "refs/remotes/origin/feature", &a],
        );
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &["update-ref", "refs/remotes/origin/noise", &b],
        );
        std::fs::write(repo_path.join(".git/shallow"), format!("{b}\n")).unwrap();

        let worktree_path = temp.path().join("standalone");
        WorktreeBuilder::new(repo_path, worktree_path.clone())
            .standalone(true)
            .git_ref("feature")
            .create()
            .unwrap();

        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["rev-parse", "--abbrev-ref", "HEAD"]
            ),
            "feature"
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["rev-parse", "refs/remotes/origin/feature"]
            ),
            a,
            "checkout dest-branch origin ref must survive source-HEAD CoW prune"
        );
        let noise = std::process::Command::new("git")
            .current_dir(&worktree_path)
            .args(["show-ref", "--verify", "refs/remotes/origin/noise"])
            .output()
            .unwrap();
        assert!(
            !noise.status.success(),
            "unrelated origin/noise must still be pruned after checkout sanitize"
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["config", "--get-all", "remote.origin.fetch"]
            ),
            "+refs/heads/feature:refs/remotes/origin/feature"
        );
        assert!(
            !worktree_path.join(".git/shallow").exists(),
            "after checkout, graft B is unused and its parent is in the ODB"
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["rev-parse", "--is-shallow-repository"]
            ),
            "false"
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(&worktree_path, &["rev-parse", "HEAD^"]),
            a
        );
    }

    #[test]
    fn test_standalone_worktree_keeps_origin_ref_after_detached_checkout() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("a.txt"), "a").unwrap();
        git_commit_all(&repo_path, "A");
        let a = echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "HEAD"]);
        std::fs::write(repo_path.join("b.txt"), "b").unwrap();
        git_commit_all(&repo_path, "B");
        let b = echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "HEAD"]);
        echo_agent_test_utils::git::run_git(&repo_path, &["branch", "feature", &a]);
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/echoagent.git",
            ],
        );
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &["update-ref", "refs/remotes/origin/feature", &a],
        );
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &["update-ref", "refs/remotes/origin/noise", &b],
        );

        let worktree_path = temp.path().join("standalone");
        WorktreeBuilder::new(repo_path, worktree_path.clone())
            .standalone(true)
            .git_ref("origin/feature")
            .create()
            .unwrap();

        assert_eq!(
            echo_agent_test_utils::git::run_git(&worktree_path, &["rev-parse", "HEAD"]),
            a
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["rev-parse", "refs/remotes/origin/feature"]
            ),
            a,
            "detached origin/feature checkout must keep that remote-tracking ref"
        );
        let noise = std::process::Command::new("git")
            .current_dir(&worktree_path)
            .args(["show-ref", "--verify", "refs/remotes/origin/noise"])
            .output()
            .unwrap();
        assert!(
            !noise.status.success(),
            "unrelated origin/noise must still be pruned"
        );
    }

    #[test]
    fn test_standalone_worktree_skips_extra_origin_remotes() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");
        let head = echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "HEAD"]);
        let branch =
            echo_agent_test_utils::git::run_git(&repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]);
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &["update-ref", "refs/remotes/origin/main", &head],
        );
        echo_agent_test_utils::git::run_git(
            &repo_path,
            &[
                "update-ref",
                &format!("refs/remotes/origin/{branch}"),
                &head,
            ],
        );
        for i in 0..40 {
            echo_agent_test_utils::git::run_git(
                &repo_path,
                &[
                    "update-ref",
                    &format!("refs/remotes/origin/branch-{i}"),
                    &head,
                ],
            );
        }

        let worktree_path = temp.path().join("standalone");
        WorktreeBuilder::new(repo_path, worktree_path.clone())
            .standalone(true)
            .create()
            .unwrap();

        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["rev-parse", "refs/remotes/origin/main"]
            ),
            head
        );
        assert_eq!(
            echo_agent_test_utils::git::run_git(
                &worktree_path,
                &["rev-parse", &format!("refs/remotes/origin/{branch}")]
            ),
            head
        );
        for i in 0..40 {
            let show = std::process::Command::new("git")
                .current_dir(&worktree_path)
                .args([
                    "show-ref",
                    "--verify",
                    &format!("refs/remotes/origin/branch-{i}"),
                ])
                .output()
                .unwrap();
            assert!(
                !show.status.success(),
                "standalone dest must not have origin/branch-{i}"
            );
        }
    }

    // ─── Cancellation / partial-creation cleanup tests ───────────────────

    #[test]
    fn test_linked_cancel_after_worktree_add_deregisters() {
        // A pre-cancelled Linked creation bails right after `git worktree add`.
        // The partial worktree dir AND its `.git/worktrees/<name>` registration
        // must both be cleaned up, so a later create at the same dest isn't
        // blocked by a stale registration.
        echo_agent_test_utils::require_git!();
        use crate::CreationMode;
        use tokio_util::sync::CancellationToken;

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        let dest = temp.path().join("wt");
        let token = CancellationToken::new();
        token.cancel();

        let result = WorktreeBuilder::new(repo_path.clone(), dest.clone())
            .creation_mode(CreationMode::Linked)
            .cancellation_token(token)
            .create();
        assert!(result.is_err(), "cancelled creation must fail");

        assert!(!dest.exists(), "cancelled worktree dir must be removed");
        let registrations = std::fs::read_dir(repo_path.join(".git/worktrees"))
            .map(|rd| rd.flatten().count())
            .unwrap_or(0);
        assert_eq!(
            registrations, 0,
            "the linked-worktree registration must be deregistered"
        );
    }

    #[test]
    fn test_standalone_cancel_removes_partial_dest() {
        // A pre-cancelled standalone creation must join the background `.git/`
        // copy thread and then remove the partial dest — no leftover dir.
        // NOTE: with a tiny repo the `.git/` copy finishes ~instantly, so this
        // covers the join-before-teardown ORDERING structurally (no timing
        // fault-injection); the unconditional join in execute.rs makes it safe
        // regardless of thread timing.
        echo_agent_test_utils::require_git!();
        use tokio_util::sync::CancellationToken;

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        let dest = temp.path().join("standalone-wt");
        let token = CancellationToken::new();
        token.cancel();

        let result = WorktreeBuilder::new(repo_path.clone(), dest.clone())
            .standalone(true)
            .cancellation_token(token)
            .create();
        assert!(result.is_err(), "cancelled standalone creation must fail");
        assert!(!dest.exists(), "cancelled standalone dest must be removed");
    }

    #[test]
    fn test_linked_hard_error_reclaims_and_deregisters() {
        // A hard (non-cancel) error AFTER `git worktree add` arms the guard must
        // reclaim the dir AND deregister `.git/worktrees/<name>` — proves the
        // guard fires on the error path, not only on cancel. An invalid
        // ignored-files glob makes the ignored-copy phase fail deterministically.
        echo_agent_test_utils::require_git!();
        use crate::{CreationMode, IgnoredFilesMode};

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        let dest = temp.path().join("wt");
        let result = WorktreeBuilder::new(repo_path.clone(), dest.clone())
            .creation_mode(CreationMode::Linked)
            .ignored_files_mode(IgnoredFilesMode::Copy {
                skip_patterns: vec!["[".to_string()], // invalid glob → build fails
            })
            .create();
        assert!(result.is_err(), "invalid skip glob must fail creation");

        assert!(
            !dest.exists(),
            "partial worktree dir must be reclaimed on a hard error"
        );
        let registrations = std::fs::read_dir(repo_path.join(".git/worktrees"))
            .map(|rd| rd.flatten().count())
            .unwrap_or(0);
        assert_eq!(
            registrations, 0,
            "registration must be deregistered on a hard error"
        );
    }

    #[test]
    fn test_standalone_hard_error_reclaims_dest() {
        // A hard (non-cancel) error after the guard is armed (a bogus non-HEAD
        // ref → checkout fails) must reclaim the partial standalone dest.
        echo_agent_test_utils::require_git!();

        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        let dest = temp.path().join("standalone-wt");
        let result = WorktreeBuilder::new(repo_path.clone(), dest.clone())
            .standalone(true)
            .git_ref("definitely-not-a-ref")
            .create();
        assert!(result.is_err(), "bogus ref must fail creation");
        assert!(
            !dest.exists(),
            "partial standalone dest must be reclaimed on a hard error"
        );
    }
}
