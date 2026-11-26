use super::{DbStats, GcReport, RebuildReport};
use crate::fs_size::{Volume, physical_dir_size};
use crate::util::{format_age, format_bytes, pad_to_width, truncate_to_width, unix_now};
use echo_agent_fast_worktree::WorktreeRecord;
use std::io::Write;
use std::path::Path;
use unicode_width::UnicodeWidthStr;
const REPO_WIDTH: usize = 6;
const BRANCH_WIDTH: usize = 20;
const AGE_WIDTH: usize = 10;
/// Truncate-then-pad to exactly `width` display columns; headers and data
/// share it so the two stay aligned.
fn cell(s: &str, width: usize) -> String {
    pad_to_width(&truncate_to_width(s, width), width)
}
pub fn print_table(records: &[WorktreeRecord], out: &mut impl Write) -> std::io::Result<()> {
    if records.is_empty() {
        writeln!(out, "No worktrees found.")?;
        return Ok(());
    }
    let id_width = records
        .iter()
        .map(|r| UnicodeWidthStr::width(r.id.as_str()))
        .max()
        .unwrap_or(0)
        .max(16);
    let label_width = records
        .iter()
        .map(|r| r.label().map_or(0, UnicodeWidthStr::width))
        .max()
        .unwrap_or(0)
        .clamp(5, 24);
    let type_width = records
        .iter()
        .map(|r| UnicodeWidthStr::width(r.kind.as_str()))
        .fold(UnicodeWidthStr::width("TYPE"), usize::max);
    writeln!(
        out,
        "  {} {} {} {} {} {:<AGE_WIDTH$} PATH",
        pad_to_width("ID", id_width),
        cell("TYPE", type_width),
        cell("REPO", REPO_WIDTH),
        cell("LABEL", label_width),
        cell("BRANCH", BRANCH_WIDTH),
        "AGE",
    )?;
    let now = unix_now();
    for rec in records {
        let age = format_age(rec.created_at, now);
        let branch = rec.git_ref.as_deref().unwrap_or("(detached)");
        let label = rec.label().unwrap_or("");
        let path = abbreviate_home(&rec.path);
        writeln!(
            out,
            "  {} {} {} {} {} {:<AGE_WIDTH$} {}",
            pad_to_width(&rec.id, id_width),
            cell(rec.kind.as_str(), type_width),
            cell(&rec.repo_name, REPO_WIDTH),
            cell(label, label_width),
            cell(branch, BRANCH_WIDTH),
            age,
            path,
        )?;
    }
    let total = records.len();
    let by_kind: std::collections::BTreeMap<&str, usize> =
        records
            .iter()
            .fold(std::collections::BTreeMap::new(), |mut m, r| {
                *m.entry(r.kind.as_str()).or_default() += 1;
                m
            });
    let breakdown: Vec<String> = by_kind.iter().map(|(k, v)| format!("{v} {k}")).collect();
    writeln!(out, "  {} worktrees ({})", total, breakdown.join(", "))
}
pub fn print_json(records: &[WorktreeRecord], out: &mut impl Write) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(records).unwrap_or_else(|_| "[]".to_string());
    writeln!(out, "{json}")
}
pub fn print_show(rec: &WorktreeRecord, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "  Path:           {}", rec.path.display())?;
    writeln!(out, "  ID:             {}", rec.id)?;
    writeln!(out, "  Type:           {}", rec.kind.as_str())?;
    writeln!(out, "  Source Repo:    {}", rec.source_repo.display())?;
    writeln!(out, "  Creation Mode:  {}", rec.creation_mode)?;
    if let Some(ref git_ref) = rec.git_ref {
        writeln!(out, "  Git Ref:        {git_ref}")?;
    }
    if let Some(ref commit) = rec.head_commit {
        let short = if commit.len() > 12 {
            &commit[..12]
        } else {
            commit
        };
        writeln!(out, "  HEAD:           {short}")?;
    }
    writeln!(
        out,
        "  Created:        {}",
        format_timestamp(rec.created_at)
    )?;
    if let Some(ts) = rec.last_accessed_at {
        writeln!(out, "  Last Accessed:  {}", format_timestamp(ts))?;
    }
    if let Some(ref sid) = rec.session_id {
        writeln!(out, "  Session ID:     {sid}")?;
    }
    if let Some(pid) = rec.creator_pid {
        writeln!(out, "  Creator PID:    {pid}")?;
    }
    writeln!(out, "  Status:         {}", rec.status.as_str())?;
    if let Some(label) = rec.label() {
        writeln!(out, "  Label:          {label}")?;
    }
    if rec.path.exists() {
        let size = physical_dir_size(&rec.path, Volume::of(&rec.path));
        let bytes = size.measure.bytes().unwrap_or_default();
        write!(out, "  Disk Usage:     {}", format_bytes(bytes))?;
        let skipped = size.issues.skipped();
        if skipped > 0 {
            let noun = if skipped == 1 { "entry" } else { "entries" };
            write!(out, " ({skipped} {noun} skipped)")?;
        }
        writeln!(out)?;
    }
    Ok(())
}
pub fn print_stats(stats: &DbStats, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "Worktree DB Statistics")?;
    writeln!(out, "======================")?;
    writeln!(out, "  Total records:  {}", stats.total_records)?;
    writeln!(out, "  Alive:          {}", stats.alive_count)?;
    writeln!(out, "  Dead:           {}", stats.dead_count)?;
    writeln!(
        out,
        "  DB size:        {}",
        format_bytes(stats.db_file_bytes)
    )
}
pub fn print_gc(report: &GcReport, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "GC report:")?;
    writeln!(out, "  Dead records removed:      {}", report.dead_removed)?;
    writeln!(
        out,
        "  Expired worktrees removed: {}",
        report.expired_removed
    )?;
    if report.no_repo_paths > 0 {
        writeln!(out, "  Non-repository paths:      {}", report.no_repo_paths)?;
    }
    writeln!(out, "  Skipped (guarded):         {}", report.skipped_alive)?;
    if report.never_expiring > 0 {
        writeln!(
            out,
            "  Kept (never expires):      {}",
            report.never_expiring
        )?;
    }
    if report.kept_unsafe > 0 {
        writeln!(out, "  Kept (not reclaimable):    {}", report.kept_unsafe)?;
        for (reason, count) in &report.kept_reasons {
            writeln!(out, "    {reason}: {count}")?;
        }
        const MAX_KEPT_PRINTED: usize = 20;
        let printed = report.kept.len().min(MAX_KEPT_PRINTED);
        for kept in report.kept.iter().take(MAX_KEPT_PRINTED) {
            writeln!(out, "      {}  ({})", kept.path, kept.reason)?;
        }
        let rest = usize::try_from(report.kept_unsafe)
            .unwrap_or(usize::MAX)
            .saturating_sub(printed);
        if rest > 0 {
            writeln!(out, "      and {rest} more, named in the log")?;
        }
    }
    if report.names_collected > 0 {
        writeln!(
            out,
            "  Reclaimed names dropped:   {}",
            report.names_collected
        )?;
    }
    if report.not_judged > 0 {
        writeln!(out, "  Not judged this pass:      {}", report.not_judged)?;
    }
    if report.unnamed > 0 {
        writeln!(out, "  Naming failed (kept):      {}", report.unnamed)?;
    }
    if report.remove_failed > 0 {
        writeln!(out, "  Removal failures:          {}", report.remove_failed)?;
    }
    Ok(())
}
pub fn print_rebuild(report: &RebuildReport, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "Rebuild report:")?;
    writeln!(out, "  Discovered:      {}", report.discovered)?;
    writeln!(out, "  Registered:      {}", report.registered)?;
    writeln!(out, "  Already tracked: {}", report.already_tracked)
}
fn format_timestamp(ts: i64) -> String {
    let dt = chrono::DateTime::from_timestamp(ts, 0);
    match dt {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        None => ts.to_string(),
    }
}
fn abbreviate_home(path: &Path) -> String {
    crate::util::abbreviate_path(&path.to_string_lossy()).into_owned()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn print_table_never_truncates_long_ids() {
        let long_id = "a".repeat(40);
        let mut out = Vec::new();
        print_table(&[make_record(&long_id, "lbl")], &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains(&long_id), "full ID must be present: {text}");
    }
    fn make_record(id: &str, label: &str) -> WorktreeRecord {
        crate::test_util::make_worktree_record(
            id,
            std::path::Path::new(&format!("/tmp/wt-{id}")),
            label,
        )
    }
    #[test]
    fn print_show_non_nfs_omits_nfs_block() {
        let rec = make_record("wt-copy", "c");
        let mut out = Vec::new();
        print_show(&rec, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("Strategy:       nfs"), "{text}");
        assert!(!text.contains("clean-artifacts"), "{text}");
    }
    #[test]
    fn print_table_pads_cjk_labels_by_display_width() {
        let records = vec![
            make_record("wt-cjk", "组件更新"),
            make_record("wt-ascii", "plain-label"),
        ];
        let mut out = Vec::new();
        print_table(&records, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("组件更新"));
        crate::test_util::assert_path_column_aligned(&text, "/tmp/wt-");
    }
}
