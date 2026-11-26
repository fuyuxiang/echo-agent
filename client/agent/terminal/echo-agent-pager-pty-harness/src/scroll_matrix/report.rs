//! Cell verdicts: the `report.json` artifact, the stdout summary table, and
//! the exit-code policy shared by the curated CI tests and the
//! `scroll-matrix` sweep binary.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

/// Verdict of one invariant row within a cell run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantStatus {
    /// Held, and was expected to hold.
    Pass,
    /// Violated outside the cell's xfail set.
    Fail,
    /// Violated inside the xfail set (the declared known bug).
    XFail,
    /// Held despite being in the xfail set — the bug got fixed or the cell
    /// rotted; either way the cell must be promoted out of xfail, so this
    /// fails the run exactly like [`InvariantStatus::Fail`].
    XPass,
}

/// Cell-level verdict: the worst of its invariant rows
/// (`Fail > XPass > XFail > Pass` — see `runner::classify`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CellStatus {
    Pass,
    Fail,
    XFail,
    XPass,
}

impl CellStatus {
    /// Fixed-width table label.
    pub fn as_str(self) -> &'static str {
        match self {
            CellStatus::Pass => "PASS",
            CellStatus::Fail => "FAIL",
            CellStatus::XFail => "XFAIL",
            CellStatus::XPass => "XPASS",
        }
    }
}

/// One invariant row of a [`CellReport`].
#[derive(Clone, Debug, Serialize)]
pub struct InvariantReport {
    /// Design vocabulary id (`I-ORD`, …).
    pub id: String,
    pub status: InvariantStatus,
    /// Violation detail, or the promote-out-of-xfail note on XPass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Verdict of one matrix cell run.
#[derive(Clone, Debug, Serialize)]
pub struct CellReport {
    pub cell_id: String,
    /// `curated` | `full`.
    pub tier: String,
    pub status: CellStatus,
    /// One row per declared invariant; empty when the run aborted before
    /// evaluation (see `note`).
    pub invariants: Vec<InvariantReport>,
    /// The cell's `ECHO_AGENT_SCROLL_LOG` capture (kept for post-mortems).
    pub log_path: String,
    /// Streams grouped out of the capture (finalized + trailing in-flight).
    pub streams: usize,
    pub duration_ms: u64,
    /// Phase note for runs that never reached invariant evaluation (setup
    /// panic, finalize-wait timeout, the per-cell hard cap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Exit-code policy: nonzero iff any cell **Fail** or **XPass** — expected
/// failures (XFail) are green, but a fixed-or-rotted xfail cell must break
/// the run so it gets promoted instead of silently absorbed.
pub fn exit_code(reports: &[CellReport]) -> u8 {
    let failed = reports
        .iter()
        .any(|r| matches!(r.status, CellStatus::Fail | CellStatus::XPass));
    u8::from(failed)
}

/// Write `report.json` (pretty, array of [`CellReport`]) into `dir`,
/// creating it as needed; returns the file path.
pub fn write_report_json(reports: &[CellReport], dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create artifacts dir {}", dir.display()))?;
    let path = dir.join("report.json");
    let json = serde_json::to_string_pretty(reports).context("serialize cell reports")?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Detail column source: the run's phase note, else the first non-Pass
/// invariant row's `id: detail`.
fn detail_for(report: &CellReport) -> String {
    if let Some(note) = &report.note {
        return note.clone();
    }
    report
        .invariants
        .iter()
        .find(|row| row.status != InvariantStatus::Pass)
        .map(|row| {
            let detail = row.detail.as_deref().unwrap_or("");
            format!("{}: {detail}", row.id)
        })
        .unwrap_or_else(|| "-".to_owned())
}

/// Truncation width for the table's detail column (full text lives in
/// `report.json`).
const DETAIL_WIDTH: usize = 72;

/// Render the aligned one-row-per-cell summary table. Plain ASCII, no
/// ANSI/TTY dependence — safe for CI logs and `| tee`.
pub fn summary_table(reports: &[CellReport]) -> String {
    let mut rows: Vec<[String; 6]> = vec![[
        "CELL".into(),
        "TIER".into(),
        "STATUS".into(),
        "STREAMS".into(),
        "TIME".into(),
        "DETAIL".into(),
    ]];
    for report in reports {
        let mut detail = detail_for(report).replace(['\n', '\r'], " ");
        if detail.len() > DETAIL_WIDTH {
            let cut = (0..=DETAIL_WIDTH.saturating_sub(3))
                .rev()
                .find(|&i| detail.is_char_boundary(i))
                .unwrap_or(0);
            detail.truncate(cut);
            detail.push_str("...");
        }
        rows.push([
            report.cell_id.clone(),
            report.tier.clone(),
            report.status.as_str().to_owned(),
            report.streams.to_string(),
            format!("{}ms", report.duration_ms),
            detail,
        ]);
    }

    let mut widths = [0usize; 6];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }
    let mut out = String::new();
    for row in &rows {
        let mut line = String::new();
        for (width, cell) in widths.iter().zip(row) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(cell_id: &str, status: CellStatus, rows: Vec<InvariantReport>) -> CellReport {
        CellReport {
            cell_id: cell_id.to_owned(),
            tier: "curated".to_owned(),
            status,
            invariants: rows,
            log_path: format!("/tmp/{cell_id}.jsonl"),
            streams: 1,
            duration_ms: 1234,
            note: None,
        }
    }

    fn row(id: &str, status: InvariantStatus, detail: Option<&str>) -> InvariantReport {
        InvariantReport {
            id: id.to_owned(),
            status,
            detail: detail.map(str::to_owned),
        }
    }

    #[test]
    fn exit_code_is_nonzero_iff_fail_or_xpass() {
        let pass = report("a", CellStatus::Pass, vec![]);
        let xfail = report("b", CellStatus::XFail, vec![]);
        let fail = report("c", CellStatus::Fail, vec![]);
        let xpass = report("d", CellStatus::XPass, vec![]);

        assert_eq!(exit_code(&[]), 0);
        assert_eq!(
            exit_code(&[pass.clone(), xfail.clone()]),
            0,
            "XFail is green"
        );
        assert_eq!(exit_code(&[pass.clone(), fail]), 1);
        // The fixed-bug tripwire: an xfail cell that PASSES must break the run.
        assert_eq!(exit_code(&[pass, xfail, xpass]), 1, "XPass must be nonzero");
    }

    #[test]
    fn report_json_shape_and_optional_fields() {
        let full = report(
            "cell_x",
            CellStatus::XFail,
            vec![row("I-NO-DROP", InvariantStatus::XFail, Some("dropped 74"))],
        );
        let value = serde_json::to_value(&full).expect("serialize");
        assert_eq!(value["cell_id"], "cell_x");
        assert_eq!(value["tier"], "curated");
        assert_eq!(value["status"], "x_fail", "snake_case enum labels");
        assert_eq!(value["streams"], 1);
        assert_eq!(value["duration_ms"], 1234);
        assert_eq!(value["invariants"][0]["id"], "I-NO-DROP");
        assert_eq!(value["invariants"][0]["status"], "x_fail");
        assert_eq!(value["invariants"][0]["detail"], "dropped 74");
        assert!(
            value.get("note").is_none(),
            "None note must not serialize: {value}"
        );

        let pass_row = serde_json::to_value(row("I-ORD", InvariantStatus::Pass, None)).unwrap();
        assert!(pass_row.get("detail").is_none(), "None detail skipped");
    }

    #[test]
    fn write_report_json_creates_dir_and_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("artifacts");
        let path = write_report_json(&[report("a", CellStatus::Pass, vec![])], &nested)
            .expect("write report");
        assert_eq!(path, nested.join("report.json"));
        let raw = std::fs::read_to_string(&path).expect("read back");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed[0]["cell_id"], "a");
    }

    #[test]
    fn summary_table_aligns_columns_and_stays_single_line_per_cell() {
        let reports = vec![
            report("short", CellStatus::Pass, vec![]),
            report(
                "a_much_longer_cell_identifier",
                CellStatus::Fail,
                vec![row(
                    "I-CAP",
                    InvariantStatus::Fail,
                    Some(&"flushed 40 exceeds cap 25\nwith a newline".repeat(4)),
                )],
            ),
        ];
        let table = summary_table(&reports);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3, "header + one row per cell:\n{table}");

        // Every row's STATUS column starts where the header's does.
        let status_col = lines[0].find("STATUS").expect("header STATUS");
        assert_eq!(&lines[1][status_col..status_col + 4], "PASS");
        assert_eq!(&lines[2][status_col..status_col + 4], "FAIL");

        // Failure detail is carried (truncated, newlines flattened).
        assert!(lines[2].contains("I-CAP: flushed 40"), "table:\n{table}");
        assert!(lines[2].contains("..."), "long detail truncated:\n{table}");
        assert!(
            !table.contains('\x1b'),
            "table must be plain ASCII (non-TTY-safe)"
        );
    }
}
