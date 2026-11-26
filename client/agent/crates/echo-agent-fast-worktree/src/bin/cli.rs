//! CLI for fast git worktree creation.
//!
//! Usage:
//!   fast-worktree create <source> <dest> [options]
//!
//! Example:
//!   fast-worktree create /path/to/repo /path/to/worktree --dirty --parallelism 8

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use tracing::{Level, info};

use echo_agent_fast_worktree::{BtrfsMode, IgnoredFilesMode, WorkingTreeMode, WorktreeBuilder};

/// CLI enum for BTRFS mode selection
#[derive(Clone, Debug, Default, ValueEnum)]
enum CliBtrfsMode {
    /// Auto-detect: use BTRFS snapshot if source is on a BTRFS subvolume
    #[default]
    Auto,
    /// Force BTRFS snapshot (error if not available)
    Force,
    /// Disable BTRFS snapshot, always use file-by-file copy
    Disabled,
}

impl From<CliBtrfsMode> for BtrfsMode {
    fn from(mode: CliBtrfsMode) -> Self {
        match mode {
            CliBtrfsMode::Auto => BtrfsMode::Auto,
            CliBtrfsMode::Force => BtrfsMode::Force,
            CliBtrfsMode::Disabled => BtrfsMode::Disabled,
        }
    }
}

#[derive(Parser)]
#[command(name = "fast-worktree")]
#[command(about = "High-performance git worktree creation using CoW cloning")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new worktree from source
    Create {
        /// Source repository or worktree path
        source: PathBuf,

        /// Destination path for the new worktree
        dest: PathBuf,

        /// Git ref to checkout (default: HEAD)
        #[arg(long, default_value = "HEAD")]
        git_ref: String,

        /// Copy dirty/modified files from source
        #[arg(long, short = 'd')]
        dirty: bool,

        /// Copy ignored files (node_modules, target, etc.)
        #[arg(long, short = 'i')]
        ignored: bool,

        /// Number of parallel workers (0 = auto)
        #[arg(long, short = 'j', default_value = "0")]
        parallelism: usize,

        /// Parallelism for ignored files copy
        #[arg(long, default_value = "0")]
        ignored_parallelism: usize,

        /// Patterns to skip when copying ignored files
        #[arg(long)]
        skip: Vec<String>,

        /// BTRFS snapshot mode (Linux only): auto, force, or disabled
        #[arg(long, value_enum, default_value = "auto")]
        btrfs: CliBtrfsMode,

        /// Create a standalone repo copy instead of a linked worktree.
        /// The copy has its own .git/ (CoW'd) and can be promoted via rename.
        #[arg(long, short = 's')]
        standalone: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing
    let level = if cli.verbose {
        Level::DEBUG
    } else {
        Level::INFO
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .init();

    match cli.command {
        Commands::Create {
            source,
            dest,
            git_ref,
            dirty,
            ignored,
            parallelism,
            ignored_parallelism,
            skip,
            btrfs,
            standalone,
        } => {
            let start = Instant::now();

            info!(
                source = %source.display(),
                dest = %dest.display(),
                git_ref = %git_ref,
                dirty = dirty,
                ignored = ignored,
                parallelism = parallelism,
                btrfs = ?btrfs,
                standalone = standalone,
                "Creating worktree"
            );

            let working_tree = if dirty {
                WorkingTreeMode::PreserveWorkingTree
            } else {
                WorkingTreeMode::CleanAll
            };

            let ignored_files = if ignored {
                IgnoredFilesMode::Copy {
                    skip_patterns: skip,
                }
            } else {
                IgnoredFilesMode::Skip
            };

            let result = WorktreeBuilder::new(source, dest)
                .git_ref(git_ref)
                .parallelism(parallelism)
                .ignored_parallelism(ignored_parallelism)
                .channel_buffer(1024)
                .working_tree_mode(working_tree)
                .ignored_files_mode(ignored_files)
                .btrfs_mode(btrfs.into())
                .standalone(standalone)
                .create()?;

            let elapsed = start.elapsed();

            println!("\n✓ Worktree created successfully!");
            println!("  Path:   {}", result.worktree_path.display());
            println!("  Commit: {}", &result.commit[..12]);

            // For snapshot methods (btrfs/overlay), files_copied will be 0
            if result.unignored_copy.files_copied > 0 {
                println!(
                    "  Files:  {} copied, {} dirs",
                    result.unignored_copy.files_copied, result.unignored_copy.dirs_created
                );
            } else {
                println!("  Method: snapshot (instant, BTRFS or overlay)");
            }

            if standalone {
                println!("  Mode:   standalone (independent .git/, promotable via rename)");
            } else {
                println!("  Mode:   linked worktree");
            }

            if let Some(ref ignored_stats) = result.ignored_copy {
                println!("  Ignored: {} copied", ignored_stats.files_copied);
            }

            let mut printed_warnings = false;
            if !result.unignored_copy.issues.is_empty() {
                println!("\n⚠ Warnings:");
                printed_warnings = true;
                for error in &result.unignored_copy.issues {
                    println!("  - {}", error);
                }
            }
            if let Some(ref ignored_stats) = result.ignored_copy
                && !ignored_stats.issues.is_empty()
            {
                if !printed_warnings {
                    println!("\n⚠ Warnings:");
                }
                for error in &ignored_stats.issues {
                    println!("  - {}", error);
                }
            }

            println!("\n  Time: {:.2?}", elapsed);
        }
    }

    Ok(())
}
