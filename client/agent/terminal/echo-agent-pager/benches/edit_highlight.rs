//! Criterion benchmarks: edit-diff syntax-highlight strategy costs.
//!
//! | Strategy | What runs |
//! |----------|-----------|
//! | `hunk_only` | Prod cold path: [`render_diff_hunks_highlighted`] |
//! | `full_file_slice` | Prod upgrade compute: [`compute_file_scoped_styles`] |
//! | `upgrade_once_per_file` | **Each iter:** full-file compute + one paint with those styles |
//! | `paint_with_precomputed` | Styles computed in setup; timed path is paint only |
//! | `prefix_per_hunk` | Non-product baseline: silent-prime `1..hunk_start` per hunk (small fixture only) |
//!
//! Groups: `edit_hl/matrix` (500L+prefix, 10kL no prefix) and `edit_hl/upgrade`
//! (amortized session paint vs one-shot upgrade vs cold control).
//!
//! Caps: 2 MiB / 50k lines — see product docs on the edit block / worker.
//! Magnitudes: run this bench; do not treat module docs as gates.
//!
//! `compute_file_scoped_styles` stops at the last hunk line; these fixtures
//! spread hunks to near EOF, so `full_file_slice` still measures ≈ the whole
//! file (the worst case a real upgrade pays).
//!
//! ```text
//! cargo bench -p echo-agent-pager --bench edit_highlight
//! cargo bench -p echo-agent-pager --bench edit_highlight -- edit_hl/upgrade
//! ```

use std::collections::HashMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use similar::ChangeTag;
use syntect::highlighting::Style as SyntectStyle;
use tempfile::TempDir;

use echo_agent_pager::scrollback::blocks::tool::{
    DiffRenderConfig, EDIT_HL_MAX_BYTES, EDIT_HL_MAX_LINES, compute_file_scoped_styles,
    render_diff_hunks_highlighted, render_diff_hunks_with_styles,
};
use echo_agent_pager::syntax::{Syntect, get_syntect};
use echo_agent_pager::theme::Theme;
use echo_agent_pager_diff::{DiffHunk, DiffLine};

const SAMPLE_SIZE: usize = 20;
const SAMPLE_SIZE_HEAVY: usize = 10;
const WARMUP_TIME: Duration = Duration::from_secs(1);
const MEASURE_TIME: Duration = Duration::from_secs(3);
const MEASURE_TIME_HEAVY: Duration = Duration::from_secs(5);
const RENDER_WIDTH: u16 = 120;

// ── Fixtures ────────────────────────────────────────────────────────────────

struct Fixture {
    _dir: TempDir,
    path: PathBuf,
    file_text: String,
    hunks: Vec<DiffHunk>,
    label: String,
}

impl Fixture {
    fn path(&self) -> &Path {
        &self.path
    }

    fn bytes(&self) -> u64 {
        self.file_text.len() as u64
    }

    fn n_hunks(&self) -> usize {
        self.hunks.len()
    }
}

/// Generated Python with mid-file `"""` closers. Hunk text matches disk (post-edit).
fn gen_python_fixture(n_lines: usize, n_hunks: usize) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("bulky_{n_lines}.py"));

    let mut body = String::with_capacity(n_lines * 40);
    body.push_str("\"\"\"Module docstring: bulky fixture for edit highlight benches.\"\"\"\n");
    body.push_str("from pydantic import Field\n\n");

    let classes = (n_lines / 10).max(n_hunks.max(8));
    for i in 0..classes {
        body.push_str(&format!(
            "class Class_{i}:\n\
             \t\"\"\"Docstring for Class_{i}.\n\
             \n\
             \tExtra lines so the closer is mid-block.\n\
             \t\"\"\"\n\
             \tfield_a: int = {i}\n\
             \tfield_b: str = Field(...)\n\
             \tfield_c: list[str] = Field(default_factory=list)\n\
             \n"
        ));
    }
    let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
    while lines.len() < n_lines {
        lines.push(format!("# pad line {}", lines.len()));
    }
    lines.truncate(n_lines);
    let file_text = lines.join("\n") + "\n";
    std::fs::write(&path, &file_text).expect("write fixture");

    let file_line_refs: Vec<&str> = file_text.lines().collect();
    let usable: Vec<usize> = file_line_refs
        .iter()
        .enumerate()
        .filter_map(|(i, l)| (l.trim() == "\"\"\"").then_some(i))
        .filter(|&i| i > n_lines / 10 && i + 6 < n_lines)
        .collect();
    assert!(
        usable.len() >= n_hunks,
        "need ≥{n_hunks} mid-file docstring closers, got {}",
        usable.len()
    );

    let mut hunks = Vec::with_capacity(n_hunks);
    for h in 0..n_hunks {
        let idx = usable[h * usable.len() / n_hunks];
        hunks.push(make_hunk_at(&file_line_refs, idx).expect("hunk at mid-file closer"));
    }

    let label = format!("py_{n_lines}L_{n_hunks}H");
    eprintln!(
        "[fixture] {label} path={} lines={} hunks={} bytes={} (caps: {} MiB / {} lines)",
        path.display(),
        n_lines,
        n_hunks,
        file_text.len(),
        EDIT_HL_MAX_BYTES / (1024 * 1024),
        EDIT_HL_MAX_LINES,
    );

    Fixture {
        _dir: dir,
        path,
        file_text,
        hunks,
        label,
    }
}

fn make_hunk_at(file_lines: &[&str], close_i: usize) -> Option<DiffHunk> {
    if close_i + 6 >= file_lines.len() {
        return None;
    }
    let start = close_i.saturating_sub(1);
    let end = (close_i + 6).min(file_lines.len());
    let mut hunk = Vec::new();
    for idx in start..end {
        let ln = idx + 1;
        let text = file_lines[idx];
        let tag = if text.contains("field_b:") {
            ChangeTag::Insert
        } else {
            ChangeTag::Equal
        };
        hunk.push(DiffLine {
            text: format!("{text}\n"),
            lo: ln,
            ln,
            tag,
        });
    }
    if hunk.iter().any(|l| l.tag != ChangeTag::Equal) {
        Some(hunk)
    } else {
        None
    }
}

// ── Non-product prefix baseline ─────────────────────────────────────────────

fn own_ranges(ranges: Vec<(SyntectStyle, &str)>) -> Vec<(SyntectStyle, String)> {
    ranges.into_iter().map(|(s, t)| (s, t.to_owned())).collect()
}

/// Silent-prime `1..hunk_start` per hunk — expensive multi-hunk baseline, not product.
fn highlight_prefix_per_hunk(
    syntect: &Syntect,
    path: &Path,
    file_lines: &[&str],
    hunks: &[DiffHunk],
) -> Vec<Vec<(SyntectStyle, String)>> {
    let mut out = Vec::new();
    for hunk in hunks {
        let mut hl = syntect.highlight_lines_by_file_path(path);
        let start_ln = hunk
            .iter()
            .filter_map(|l| {
                let n = if l.ln > 0 { l.ln } else { l.lo };
                (n > 0).then_some(n)
            })
            .min()
            .unwrap_or(1);

        if let Some(h) = hl.as_mut() {
            for line in file_lines.iter().take(start_ln.saturating_sub(1)) {
                let owned = format!("{line}\n");
                let _ = h.highlight_line(&owned, &syntect.syntax_set);
            }
        }

        for line in hunk {
            let content = line.text.trim_end_matches(['\r', '\n']);
            let owned = format!("{content}\n");
            let segs = if let Some(h) = hl.as_mut() {
                h.highlight_line(&owned, &syntect.syntax_set)
                    .map(own_ranges)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            out.push(segs);
        }
    }
    out
}

fn estimate_style_map_bytes(map: &HashMap<usize, Vec<(ratatui::style::Style, String)>>) -> usize {
    let mut bytes = std::mem::size_of_val(map);
    bytes += map.len()
        * (std::mem::size_of::<usize>()
            + std::mem::size_of::<Vec<(ratatui::style::Style, String)>>()
            + 16);
    for spans in map.values() {
        bytes += spans.capacity() * std::mem::size_of::<(ratatui::style::Style, String)>();
        for (_, s) in spans {
            bytes += s.capacity();
        }
    }
    bytes
}

// ── Bench groups ────────────────────────────────────────────────────────────

fn configure_fast(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group
        .sample_size(SAMPLE_SIZE)
        .warm_up_time(WARMUP_TIME)
        .measurement_time(MEASURE_TIME);
}

fn configure_heavy(group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>) {
    group
        .sample_size(SAMPLE_SIZE_HEAVY)
        .warm_up_time(WARMUP_TIME)
        .measurement_time(MEASURE_TIME_HEAVY);
}

fn register_prod_strategies(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    fx: &Fixture,
    theme: &Theme,
    config: &DiffRenderConfig,
) {
    group.bench_function(BenchmarkId::new("hunk_only", &fx.label), |b| {
        b.iter(|| {
            black_box(render_diff_hunks_highlighted(
                black_box(&fx.hunks),
                fx.path(),
                theme,
                RENDER_WIDTH,
                config,
            ))
        });
    });

    group.bench_function(BenchmarkId::new("full_file_slice", &fx.label), |b| {
        b.iter(|| {
            black_box(
                compute_file_scoped_styles(
                    fx.path(),
                    black_box(&fx.file_text),
                    black_box(&fx.hunks),
                )
                .expect("styles"),
            )
        });
    });
}

/// Strategy matrix: small (with prefix) + session-scale (prod paths only).
fn bench_matrix(c: &mut Criterion) {
    let theme = Theme::current();
    let config = DiffRenderConfig::default();
    let syntect = get_syntect();

    // ── 500L + prefix (own heavy config) ────────────────────────────────────
    {
        let fx = gen_python_fixture(500, 8);
        let file_lines: Vec<&str> = fx.file_text.lines().collect();
        let mut group = c.benchmark_group("edit_hl/matrix");
        configure_heavy(&mut group);
        group.throughput(Throughput::Bytes(fx.bytes()));
        register_prod_strategies(&mut group, &fx, &theme, &config);
        group.bench_function(BenchmarkId::new("prefix_per_hunk", &fx.label), |b| {
            b.iter(|| {
                black_box(highlight_prefix_per_hunk(
                    syntect,
                    fx.path(),
                    black_box(&file_lines),
                    black_box(&fx.hunks),
                ))
            });
        });
        group.finish();
    }

    // ── 10kL session (no prefix) ────────────────────────────────────────────
    {
        let fx = gen_python_fixture(10_000, 40);
        let styles = compute_file_scoped_styles(fx.path(), &fx.file_text, &fx.hunks)
            .expect("file-scoped styles for 10k fixture");
        eprintln!(
            "[memory] {} style map ≈ {:.1} KiB ({} lines retained; file {:.1} KiB)",
            fx.label,
            estimate_style_map_bytes(&styles) as f64 / 1024.0,
            styles.len(),
            fx.bytes() as f64 / 1024.0,
        );

        let mut group = c.benchmark_group("edit_hl/matrix");
        configure_heavy(&mut group);
        group.throughput(Throughput::Bytes(fx.bytes()));
        register_prod_strategies(&mut group, &fx, &theme, &config);
        group.finish();
    }
}

/// Amortized upgrade vs cold first paint on a session-scale fixture.
fn bench_upgrade(c: &mut Criterion) {
    let fx = gen_python_fixture(10_000, 40);
    let theme = Theme::current();
    let config = DiffRenderConfig::default();
    let styles =
        compute_file_scoped_styles(fx.path(), &fx.file_text, &fx.hunks).expect("upgrade styles");

    let mut group = c.benchmark_group("edit_hl/upgrade");
    configure_fast(&mut group);
    group.throughput(Throughput::Elements(fx.n_hunks() as u64));

    // Steady-state after upgrade: the shared hunk walker (per-hunk syntect,
    // same as cold) plus the map overlay for Equal/Insert lines.
    group.bench_function(BenchmarkId::new("paint_with_precomputed", &fx.label), |b| {
        b.iter(|| {
            black_box(render_diff_hunks_with_styles(
                black_box(&fx.hunks),
                fx.path(),
                black_box(&styles),
                &theme,
                RENDER_WIDTH,
                &config,
            ))
        });
    });

    // One-shot job cost: compute + paint in the same timed iteration.
    group.bench_function(BenchmarkId::new("upgrade_once_per_file", &fx.label), |b| {
        b.iter(|| {
            let map = compute_file_scoped_styles(
                fx.path(),
                black_box(&fx.file_text),
                black_box(&fx.hunks),
            )
            .expect("styles");
            black_box(render_diff_hunks_with_styles(
                black_box(&fx.hunks),
                fx.path(),
                &map,
                &theme,
                RENDER_WIDTH,
                &config,
            ))
        });
    });

    // Cold control: first paint stays hunk-only.
    group.bench_function(BenchmarkId::new("hunk_only", &fx.label), |b| {
        b.iter(|| {
            black_box(render_diff_hunks_highlighted(
                black_box(&fx.hunks),
                fx.path(),
                &theme,
                RENDER_WIDTH,
                &config,
            ))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_matrix, bench_upgrade);
criterion_main!(benches);
