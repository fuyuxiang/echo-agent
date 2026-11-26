//! Pure renderer over [`DiskUsageReport`]. `echo_agent_config::echo_agent_home()`,
//! whose first call creates the home, must stay out of this module.

use std::borrow::Cow;
use std::io::Write;
use std::path::Path;

use echo_agent_fast_worktree::WorktreeStatus;
use unicode_width::UnicodeWidthStr;

use super::{DiskUsageReport, Registration, RegistryState, WorktreeUsage};
use crate::util::{format_age, format_bytes, pad_to_width, truncate_to_width};

const SIZE_WIDTH: usize = 10;
const AGE_WIDTH: usize = 10;
const TYPE_HEADER: &str = "TYPE";
const LABEL_HEADER: &str = "LABEL";
const LABEL_WIDTH_MAX: usize = 24;

pub fn print_report(
    report: &DiskUsageReport,
    now: i64,
    out: &mut impl Write,
) -> std::io::Result<()> {
    let home_label = home_prefix_label(&report.echo_agent_home);
    writeln!(out, "Disk usage for {home_label}")?;
    for entry in &report.top_level_dirs {
        writeln!(
            out,
            "  {:>SIZE_WIDTH$}  {}",
            size_cell(entry.bytes),
            entry.name
        )?;
    }
    if report.root_files_bytes > 0 {
        writeln!(
            out,
            "  {:>SIZE_WIDTH$}  (top-level files)",
            format_bytes(report.root_files_bytes)
        )?;
    }
    writeln!(
        out,
        "  {:>SIZE_WIDTH$}  total",
        format_bytes(report.total_bytes)
    )?;
    if report.skips.unreadable_dirs > 0 {
        let (noun, pronoun) = if report.skips.unreadable_dirs == 1 {
            ("directory", "it")
        } else {
            ("directories", "them")
        };
        writeln!(
            out,
            "  {} {noun} could not be read; what is under {pronoun} may be missing from the total. RUST_LOG=debug names {pronoun}.",
            report.skips.unreadable_dirs,
        )?;
    }
    if report.skips.unstatable_entries > 0 {
        writeln!(
            out,
            "  {} {} could not be read and {} not counted.",
            report.skips.unstatable_entries,
            count_noun(report.skips.unstatable_entries, "entry", "entries"),
            count_verb(report.skips.unstatable_entries)
        )?;
    }
    if report.skips.other_filesystem_dirs > 0 {
        writeln!(
            out,
            "  {} {} on another filesystem and {} not counted, here or in any row.",
            report.skips.other_filesystem_dirs,
            count_noun(
                report.skips.other_filesystem_dirs,
                "directory is",
                "directories are"
            ),
            count_verb(report.skips.other_filesystem_dirs),
        )?;
    }
    if report.unfollowed_dir_symlinks > 0 {
        let (noun, pronoun) = if report.unfollowed_dir_symlinks == 1 {
            ("symlink to a directory is", "its")
        } else {
            ("symlinks to directories are", "their")
        };
        writeln!(
            out,
            "  {} top-level {noun} not followed, so {pronoun} contents are missing from the total.",
            report.unfollowed_dir_symlinks,
        )?;
    }
    // The proven statement replaces the general note rather than joining it.
    if report.total_exceeds_volume_used() {
        writeln!(
            out,
            "  Total exceeds the used space on this volume, so shared blocks are counted once per path."
        )?;
    } else if cfg!(unix) && !report.worktrees.is_empty() {
        writeln!(
            out,
            "  Worktree clones share storage with their source, so the total can exceed real disk use."
        )?;
    }

    writeln!(out)?;
    writeln!(out, "Worktrees")?;
    if report.worktrees_outside_managed_roots > 0 {
        writeln!(
            out,
            "  {} {} outside the managed worktree dirs {} not shown here.",
            report.worktrees_outside_managed_roots,
            count_noun(
                report.worktrees_outside_managed_roots,
                "worktree",
                "worktrees"
            ),
            count_verb(report.worktrees_outside_managed_roots)
        )?;
    }
    match report.registry {
        RegistryState::Read => {}
        RegistryState::Absent => {
            if !report.worktrees.is_empty() {
                writeln!(
                    out,
                    "  Worktree registry not found; rows may show as untracked."
                )?;
            }
        }
        RegistryState::Busy => {
            writeln!(
                out,
                "  Worktree registry is in use by another process; rows show as untracked. Retry in a moment."
            )?;
        }
        RegistryState::Unopenable => {
            writeln!(
                out,
                "  Worktree registry at {} could not be opened; rows show as untracked. Check its permissions.",
                abbreviate(&report.registry_path, &report.echo_agent_home, &home_label)
            )?;
        }
        RegistryState::Corrupt => {
            writeln!(
                out,
                "  Worktree registry is damaged; rows show as untracked. Remove {} and run `echoagent worktree db rebuild` to recreate it.",
                abbreviate(&report.registry_path, &report.echo_agent_home, &home_label)
            )?;
        }
    }
    if report.worktrees.is_empty() {
        writeln!(out, "  No worktrees found.")?;
    } else {
        let kind_cells: Vec<Cow<'_, str>> = report.worktrees.iter().map(kind_cell).collect();
        let kind_width = kind_cells
            .iter()
            .map(|k| UnicodeWidthStr::width(k.as_ref()))
            .fold(UnicodeWidthStr::width(TYPE_HEADER), usize::max);
        let label_width = report
            .worktrees
            .iter()
            .map(|w| UnicodeWidthStr::width(w.label()))
            .fold(UnicodeWidthStr::width(LABEL_HEADER), usize::max)
            .min(LABEL_WIDTH_MAX);
        writeln!(
            out,
            "  {:>SIZE_WIDTH$}  {} {:<AGE_WIDTH$} {} PATH",
            "SIZE",
            pad_to_width(TYPE_HEADER, kind_width),
            "AGE",
            pad_to_width(LABEL_HEADER, label_width),
        )?;
        for (wt, kind) in report.worktrees.iter().zip(&kind_cells) {
            let age = wt
                .age_stamp()
                .map_or_else(|| "-".to_owned(), |ts| format_age(ts, now));
            let label = truncate_to_width(wt.label(), label_width);
            writeln!(
                out,
                "  {:>SIZE_WIDTH$}  {} {:<AGE_WIDTH$} {} {}",
                size_cell(wt.bytes),
                pad_to_width(kind, kind_width),
                age,
                pad_to_width(&label, label_width),
                abbreviate(&wt.path, &report.echo_agent_home, &home_label),
            )?;
        }
    }

    // gc's age pass needs `--max-age` and walks registry records, so neither
    // half of the hint holds for both row kinds.
    if report.worktrees_dominate() && !report.worktrees.is_empty() {
        writeln!(out)?;
        if report.worktrees.iter().any(WorktreeUsage::is_tracked) {
            writeln!(
                out,
                "To reclaim space, run `echoagent worktree gc --max-age 7d --dry-run`, then the same command without `--dry-run`. Without `--max-age`, gc expires nothing, and it keeps a worktree whose work it cannot find elsewhere, naming each one."
            )?;
        }
        if !report.worktrees.iter().all(WorktreeUsage::is_tracked) {
            writeln!(
                out,
                "Untracked rows are not in the registry, so gc never visits them. Remove one with `echoagent worktree rm --dry-run <path>`, then without `--dry-run`."
            )?;
        }
    }
    Ok(())
}

pub fn print_missing_home(echo_agent_home: &str, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(
        out,
        "Nothing on disk yet at {}.",
        home_prefix_label(echo_agent_home)
    )
}

/// A dash where nothing was measured, which is not zero bytes.
fn size_cell(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "-".to_owned(), format_bytes)
}

fn count_noun(n: u64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

fn count_verb(n: u64) -> &'static str {
    if n == 1 { "is" } else { "are" }
}

fn kind_cell(wt: &WorktreeUsage) -> Cow<'static, str> {
    match &wt.registration {
        Registration::Untracked => Cow::Owned(format!("untracked ({})", wt.kind.as_str())),
        Registration::Tracked(rec) => match rec.status {
            WorktreeStatus::Dead => Cow::Owned(format!("{} (dead)", wt.kind.as_str())),
            WorktreeStatus::Alive => Cow::Borrowed(wt.kind.as_str()),
        },
    }
}

fn abbreviate(path: &str, home: &str, label: &str) -> String {
    match Path::new(path).strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => label.to_owned(),
        Ok(rest) => format!("{label}/{}", rest.display()),
        Err(_) => path.to_owned(),
    }
}

fn home_prefix_label(echo_agent_home: &str) -> String {
    crate::util::display_echo_agent_home_prefix_for(Path::new(echo_agent_home))
}
