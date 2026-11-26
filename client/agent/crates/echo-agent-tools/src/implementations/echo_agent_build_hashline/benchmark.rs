//! Offline benchmark harness for comparing anchor schemes.
//!
//! This module implements the Phase 1 (non-LLM microbenchmarks) and Phase 2
//! (deterministic edit-trace simulation) for the hashline anchor schemes.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use echo_agent_tools::implementations::echo_agent_build_hashline::benchmark::*;
//!
//! let corpus = vec![
//!     ("small.rs", "fn main() {}\n"),
//!     ("medium.rs", include_str!("fixtures/medium.rs.txt")),
//! ];
//! let report = run_benchmark(&corpus, &BenchmarkConfig::default());
//! println!("{report}");
//! ```

use std::fmt;
use std::time::Instant;

use super::anchor::split_lines;
use super::mutate::{self, LineOutcome, Mutation, apply_mutation};
use super::scheme::{
    AnchorScheme, CheckpointChain, ChunkFingerprint, ContentOnly, DEFAULT_SEARCH_RADIUS,
    ParsedAnchor, ShiftResult, ValidationResult,
};

/// Configuration for the benchmark harness.
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    /// Hash lengths to test (default: [2, 3]).
    pub hash_lengths: Vec<usize>,

    /// Chunk sizes to test for Candidate B (default: [8, 16, 32]).
    pub chunk_sizes: Vec<usize>,

    /// Checkpoint intervals to test for Candidate C (default: [16, 32, 64]).
    pub checkpoint_intervals: Vec<usize>,

    /// Search radius for shifted-anchor recovery (default: 15).
    pub search_radius: usize,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            hash_lengths: vec![2, 3],
            chunk_sizes: vec![8, 16, 32],
            checkpoint_intervals: vec![16, 32, 64],
            search_radius: DEFAULT_SEARCH_RADIUS,
        }
    }
}

/// Aggregated metrics for one scheme configuration across all corpus files
/// and all mutation scenarios.
#[derive(Debug, Clone)]
pub struct SchemeMetrics {
    /// Human-readable scheme description (e.g. "chunk_v1 hash=3 chunk=16").
    pub label: String,

    /// Total anchors validated across all scenarios.
    pub total_validations: usize,

    /// Anchors that correctly reported Valid (true positives for "unchanged").
    pub true_valid: usize,

    /// Anchors that correctly reported Stale (true positives for "changed").
    pub true_stale: usize,

    /// Anchors that reported Valid but should have been Stale (false acceptance).
    pub false_valid: usize,

    /// Anchors that reported Stale but should have been Valid (false rejection).
    pub false_stale: usize,

    /// Shifted-anchor recovery attempts.
    pub recovery_attempts: usize,

    /// Shifted-anchor recovery: found the correct shifted target.
    pub recovery_correct: usize,

    /// Shifted-anchor recovery: found a line, but not the correct target.
    pub recovery_wrong: usize,

    /// Shifted-anchor recovery: ambiguous (multiple candidates).
    pub recovery_ambiguous: usize,

    /// Shifted-anchor recovery: not found.
    pub recovery_not_found: usize,

    /// Collision count: distinct lines that produced the same anchor in the
    /// same file (local hash only).
    pub collision_count: usize,

    /// Total lines across all corpus files.
    pub total_lines: usize,

    /// Total validation time in microseconds.
    pub validation_us: u128,

    /// Number of edit-trace steps completed.
    pub trace_steps: usize,

    /// Edit-trace: steps where post-edit anchors remained valid.
    pub trace_anchors_survived: usize,

    /// Edit-trace: steps that required re-read (anchor stale after edit).
    pub trace_reread_required: usize,

    /// Estimated total read-amplification lines across all validations.
    /// Candidate A: 1 line per validation.
    /// Candidate B: chunk_size lines per validation.
    /// Candidate C: (line_idx - checkpoint_start + 1) lines per validation.
    pub read_amp_lines: usize,
}

impl SchemeMetrics {
    fn new(label: String) -> Self {
        Self {
            label,
            total_validations: 0,
            true_valid: 0,
            true_stale: 0,
            false_valid: 0,
            false_stale: 0,
            recovery_attempts: 0,
            recovery_correct: 0,
            recovery_wrong: 0,
            recovery_ambiguous: 0,
            recovery_not_found: 0,
            collision_count: 0,
            total_lines: 0,
            validation_us: 0,
            trace_steps: 0,
            trace_anchors_survived: 0,
            trace_reread_required: 0,
            read_amp_lines: 0,
        }
    }

    /// Stale detection precision: of anchors reported Stale, fraction that
    /// were truly changed.
    pub fn stale_precision(&self) -> f64 {
        let reported_stale = self.true_stale + self.false_stale;
        if reported_stale == 0 {
            return 1.0;
        }
        self.true_stale as f64 / reported_stale as f64
    }

    /// Stale detection recall: of anchors that were truly changed, fraction
    /// correctly detected as Stale.
    pub fn stale_recall(&self) -> f64 {
        let truly_changed = self.true_stale + self.false_valid;
        if truly_changed == 0 {
            return 1.0;
        }
        self.true_stale as f64 / truly_changed as f64
    }

    /// Collision rate: fraction of total lines that share an anchor with
    /// another line in the same file.
    pub fn collision_rate(&self) -> f64 {
        if self.total_lines == 0 {
            return 0.0;
        }
        self.collision_count as f64 / self.total_lines as f64
    }

    /// Average validation latency in microseconds.
    pub fn avg_validation_us(&self) -> f64 {
        if self.total_validations == 0 {
            return 0.0;
        }
        self.validation_us as f64 / self.total_validations as f64
    }

    /// Average read-amplification lines per validation.
    pub fn avg_read_amp_lines(&self) -> f64 {
        if self.total_validations == 0 {
            return 0.0;
        }
        self.read_amp_lines as f64 / self.total_validations as f64
    }

    /// Edit-trace anchor survival rate.
    pub fn trace_survival_rate(&self) -> f64 {
        if self.trace_steps == 0 {
            return 0.0;
        }
        self.trace_anchors_survived as f64 / self.trace_steps as f64
    }
}

/// Complete benchmark report across all scheme configurations.
#[derive(Debug, Clone)]
pub struct BenchmarkReport {
    /// Per-scheme metrics, one entry per configuration tested.
    pub schemes: Vec<SchemeMetrics>,

    /// Number of corpus files used.
    pub corpus_files: usize,

    /// Total lines across all corpus files.
    pub total_corpus_lines: usize,
}

impl fmt::Display for BenchmarkReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "=== Hashline Anchor Benchmark Report ===")?;
        writeln!(
            f,
            "Corpus: {} files, {} total lines\n",
            self.corpus_files, self.total_corpus_lines
        )?;

        writeln!(
            f,
            "{:<30} {:>6} {:>8} {:>8} {:>7} {:>7} {:>8} {:>8} {:>7} {:>10}",
            "Scheme",
            "Lines",
            "Collis%",
            "FalseOK",
            "Prec",
            "Recall",
            "Recov%",
            "Surv%",
            "RdAmp",
            "Avg µs"
        )?;
        writeln!(f, "{}", "-".repeat(115))?;

        for m in &self.schemes {
            let recovery_rate = if m.recovery_attempts > 0 {
                m.recovery_correct as f64 / m.recovery_attempts as f64 * 100.0
            } else {
                0.0
            };

            writeln!(
                f,
                "{:<30} {:>6} {:>7.3}% {:>8} {:>6.3} {:>6.3} {:>7.1}% {:>7.1}% {:>6.1} {:>10.2}",
                m.label,
                m.total_lines,
                m.collision_rate() * 100.0,
                m.false_valid,
                m.stale_precision(),
                m.stale_recall(),
                recovery_rate,
                m.trace_survival_rate() * 100.0,
                m.avg_read_amp_lines(),
                m.avg_validation_us(),
            )?;
        }

        Ok(())
    }
}

/// Run the full benchmark across all scheme configurations.
///
/// `corpus` is a slice of `(filename, file_content)` pairs.
pub fn run_benchmark(corpus: &[(&str, &str)], config: &BenchmarkConfig) -> BenchmarkReport {
    let mut all_metrics: Vec<SchemeMetrics> = Vec::new();

    let total_corpus_lines: usize = corpus
        .iter()
        .map(|(_, content)| split_lines(content).len())
        .sum();

    // Build scheme configurations to test.
    let schemes = build_scheme_configs(config);

    for (label, scheme) in &schemes {
        let mut metrics = SchemeMetrics::new(label.clone());

        for (name, content) in corpus {
            run_phase1_for_file(&**scheme, name, content, config, &mut metrics);
            run_phase2_for_file(&**scheme, name, content, config, &mut metrics);
        }

        all_metrics.push(metrics);
    }

    BenchmarkReport {
        schemes: all_metrics,
        corpus_files: corpus.len(),
        total_corpus_lines,
    }
}

/// Build all scheme configurations to benchmark from the config.
fn build_scheme_configs(config: &BenchmarkConfig) -> Vec<(String, Box<dyn AnchorScheme>)> {
    let mut schemes: Vec<(String, Box<dyn AnchorScheme>)> = Vec::new();

    for &hl in &config.hash_lengths {
        // Candidate A
        schemes.push((
            format!("content_only h={hl}"),
            Box::new(ContentOnly::with_hash_len(hl)),
        ));

        // Candidate B — vary chunk size
        for &cs in &config.chunk_sizes {
            schemes.push((
                format!("chunk h={hl} c={cs}"),
                Box::new(ChunkFingerprint::with_params(hl, cs)),
            ));
        }

        // Candidate C — vary checkpoint interval
        for &ci in &config.checkpoint_intervals {
            schemes.push((
                format!("checkpoint h={hl} i={ci}"),
                Box::new(CheckpointChain::with_params(hl, ci)),
            ));
        }
    }

    schemes
}

/// Standard set of mutations to apply per file.
fn standard_mutations(line_count: usize) -> Vec<(&'static str, Mutation)> {
    if line_count < 3 {
        return vec![];
    }

    let mid = line_count / 2;
    let mut mutations = vec![
        ("insert_above_mid", mutate::gen_insert_above(mid, 3)),
        ("delete_at_mid", mutate::gen_delete(mid, 2)),
        (
            "token_edit_mid",
            mutate::gen_token_edit(mid, "// EDITED LINE"),
        ),
        ("reindent_mid", mutate::gen_reindent(mid, "        ")),
        (
            "boilerplate_top",
            mutate::gen_boilerplate_insert(0, "// boilerplate", 5),
        ),
    ];

    if line_count >= 6 {
        mutations.push((
            "range_rewrite",
            mutate::gen_range_rewrite(mid, (mid + 3).min(line_count), &["// replaced"]),
        ));
    }

    mutations
}

/// Estimate the read-amplification cost for a single validation under the
/// given scheme, using the scheme's own `validation_window_lines()` method.
fn estimate_read_amp_lines(scheme: &dyn AnchorScheme, line_count: usize, line_idx: usize) -> usize {
    scheme.validation_window_lines(line_idx, line_count)
}

/// Run Phase 1 (single-mutation microbenchmarks) for one file.
fn run_phase1_for_file(
    scheme: &dyn AnchorScheme,
    _file_name: &str,
    content: &str,
    config: &BenchmarkConfig,
    metrics: &mut SchemeMetrics,
) {
    let original_lines = split_lines(content);
    let line_count = original_lines.len();
    metrics.total_lines += line_count;

    // --- Collision measurement ---
    let anchors = scheme.generate_anchors(&original_lines);
    let mut seen = std::collections::HashSet::new();
    for a in &anchors {
        let key = match &a.context {
            Some(ctx) => format!("{}:{}", a.local, ctx),
            None => a.local.clone(),
        };
        if !seen.insert(key) {
            metrics.collision_count += 1;
        }
    }

    // --- Mutation scenarios ---
    let mutations = standard_mutations(line_count);

    for (_mutation_name, mutation) in &mutations {
        let mut mutated_lines: Vec<String> = original_lines.iter().map(|s| s.to_string()).collect();
        let mutation_result = apply_mutation(&mut mutated_lines, mutation);
        let mutated_refs: Vec<&str> = mutated_lines.iter().map(|s| s.as_str()).collect();

        // For each original anchor, validate against the mutated file.
        for (orig_idx, anchor) in anchors.iter().enumerate() {
            let parsed = ParsedAnchor {
                line: anchor.line,
                local: anchor.local.clone(),
                context: anchor.context.clone(),
            };

            // Ground truth: determine expected validity based on LineOutcome.
            // An anchor should be Valid if the line is Unchanged or Reindented
            // (whitespace-normalized hashing preserves anchors across
            // indentation changes). Shifted, Modified, and Deleted anchors
            // should all be detected as invalid (Stale or OutOfRange).
            let outcome = &mutation_result.outcomes[orig_idx];
            let should_be_valid =
                matches!(outcome, LineOutcome::Unchanged | LineOutcome::Reindented);

            let t0 = Instant::now();
            let result = scheme.validate(&parsed, &mutated_refs);
            metrics.validation_us += t0.elapsed().as_micros();
            metrics.total_validations += 1;
            metrics.read_amp_lines += estimate_read_amp_lines(scheme, line_count, orig_idx);

            let reported_valid = result == ValidationResult::Valid;

            if reported_valid && should_be_valid {
                metrics.true_valid += 1;
            } else if reported_valid && !should_be_valid {
                metrics.false_valid += 1;
            } else if !reported_valid && !should_be_valid {
                metrics.true_stale += 1;
            } else {
                // !reported_valid && should_be_valid
                metrics.false_stale += 1;
            }

            // Shifted recovery: attempt when validation failed and the
            // line was shifted (not modified or deleted).
            if !reported_valid && let LineOutcome::Shifted { new_idx } = outcome {
                metrics.recovery_attempts += 1;
                let expected_line = new_idx + 1; // 1-based

                match scheme.find_shifted(&parsed, &mutated_refs, config.search_radius) {
                    ShiftResult::Found { new_line } => {
                        if new_line == expected_line {
                            metrics.recovery_correct += 1;
                        } else {
                            metrics.recovery_wrong += 1;
                        }
                    }
                    ShiftResult::Ambiguous { .. } => metrics.recovery_ambiguous += 1,
                    ShiftResult::NotFound => metrics.recovery_not_found += 1,
                }
            }
        }
    }
}

/// A single step in a deterministic edit trace.
struct TraceStep {
    mutation: Mutation,
    /// 0-based index of the anchor to probe after the edit.
    probe_anchor_idx: usize,
}

/// Standard edit traces to run per file.
fn standard_traces(line_count: usize) -> Vec<Vec<TraceStep>> {
    if line_count < 6 {
        return vec![];
    }

    let mid = line_count / 2;

    vec![
        // Trace 1: point edit followed by nearby point edit
        vec![
            TraceStep {
                mutation: mutate::gen_token_edit(mid, "// step1 edit"),
                probe_anchor_idx: mid + 1,
            },
            TraceStep {
                mutation: mutate::gen_token_edit(mid + 1, "// step2 edit"),
                probe_anchor_idx: mid + 2,
            },
        ],
        // Trace 2: insert above then probe below
        vec![
            TraceStep {
                mutation: mutate::gen_insert_above(mid, 2),
                probe_anchor_idx: mid + 1,
            },
            TraceStep {
                mutation: mutate::gen_token_edit(mid + 3, "// post-insert edit"),
                probe_anchor_idx: mid + 4,
            },
        ],
        // Trace 3: reindent (formatter pass) then edit
        vec![
            TraceStep {
                mutation: mutate::gen_reindent(mid, "        "),
                probe_anchor_idx: mid,
            },
            TraceStep {
                mutation: mutate::gen_token_edit(mid + 1, "// after reindent"),
                probe_anchor_idx: mid + 2,
            },
        ],
    ]
}

/// Run Phase 2 (edit-trace simulation) for one file.
///
/// The simulation keeps using the existing anchor set as long as the probed
/// anchor survives. Anchors are only regenerated (simulating a re-read) when
/// the probed anchor is stale. This measures how often each scheme forces a
/// re-read in sequential editing workflows.
fn run_phase2_for_file(
    scheme: &dyn AnchorScheme,
    _file_name: &str,
    content: &str,
    _config: &BenchmarkConfig,
    metrics: &mut SchemeMetrics,
) {
    let original_lines = split_lines(content);
    let line_count = original_lines.len();

    let traces = standard_traces(line_count);

    for trace in &traces {
        // Start with the original file and its anchors.
        let mut current_lines: Vec<String> = original_lines.iter().map(|s| s.to_string()).collect();
        let mut current_anchors = scheme.generate_anchors(&original_lines);
        let mut needs_refresh = false;

        for step in trace {
            // If the previous step required a re-read, regenerate anchors now.
            if needs_refresh {
                let refs: Vec<&str> = current_lines.iter().map(|s| s.as_str()).collect();
                current_anchors = scheme.generate_anchors(&refs);
                needs_refresh = false;
            }

            let probe_idx = step.probe_anchor_idx;
            if probe_idx >= current_anchors.len() {
                continue;
            }

            // Snapshot the anchor we want to probe.
            let probe_anchor = ParsedAnchor {
                line: current_anchors[probe_idx].line,
                local: current_anchors[probe_idx].local.clone(),
                context: current_anchors[probe_idx].context.clone(),
            };

            // Apply the mutation.
            apply_mutation(&mut current_lines, &step.mutation);

            // Validate the probed anchor against the mutated file.
            let refs: Vec<&str> = current_lines.iter().map(|s| s.as_str()).collect();
            let result = scheme.validate(&probe_anchor, &refs);

            metrics.trace_steps += 1;
            if result == ValidationResult::Valid {
                metrics.trace_anchors_survived += 1;
                // Keep using existing anchors — no refresh needed.
            } else {
                metrics.trace_reread_required += 1;
                // Mark for refresh at the start of the next step.
                needs_refresh = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL_FILE: &str =
        "fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{x} {y}\");\n}\n";

    const MEDIUM_FILE: &str = "\
use std::collections::HashMap;

fn process(items: &[String]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for item in items {
        *counts.entry(item.clone()).or_insert(0) += 1;
    }
    counts
}

fn format_counts(counts: &HashMap<String, usize>) -> String {
    let mut result = String::new();
    for (key, value) in counts {
        result.push_str(&format!(\"{key}: {value}\\n\"));
    }
    result
}

fn main() {
    let items = vec![
        \"apple\".to_string(),
        \"banana\".to_string(),
        \"apple\".to_string(),
        \"cherry\".to_string(),
    ];
    let counts = process(&items);
    let formatted = format_counts(&counts);
    println!(\"{formatted}\");
}
";

    const REPETITIVE_FILE: &str = "\
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct Config2 {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct Config3 {
    name: String,
    value: String,
}
";

    fn test_corpus() -> Vec<(&'static str, &'static str)> {
        vec![
            ("small.rs", SMALL_FILE),
            ("medium.rs", MEDIUM_FILE),
            ("repetitive.rs", REPETITIVE_FILE),
        ]
    }

    #[test]
    fn benchmark_runs_without_panic() {
        let corpus = test_corpus();
        let report = run_benchmark(&corpus, &BenchmarkConfig::default());
        assert!(!report.schemes.is_empty());
        assert_eq!(report.corpus_files, 3);
        assert!(report.total_corpus_lines > 0);
    }

    #[test]
    fn all_schemes_produce_metrics() {
        let corpus = test_corpus();
        let config = BenchmarkConfig::default();
        let report = run_benchmark(&corpus, &config);

        // Expected: hash_lengths.len() * (1 + chunk_sizes.len() + checkpoint_intervals.len())
        let expected = config.hash_lengths.len()
            * (1 + config.chunk_sizes.len() + config.checkpoint_intervals.len());
        assert_eq!(report.schemes.len(), expected);

        for m in &report.schemes {
            assert!(m.total_validations > 0, "no validations for {}", m.label);
            assert!(m.total_lines > 0, "no lines counted for {}", m.label);
        }
    }

    #[test]
    fn content_only_has_zero_false_stale() {
        // With proper ground truth (Unchanged = should be Valid, Shifted =
        // should be Stale), Candidate A should have zero false_stale:
        // it never reports Stale for a truly unchanged-at-same-position line
        // because it has no contextual component.
        let corpus = test_corpus();
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![],
            checkpoint_intervals: vec![],
            search_radius: DEFAULT_SEARCH_RADIUS,
        };
        let report = run_benchmark(&corpus, &config);
        assert_eq!(report.schemes.len(), 1);
        assert_eq!(
            report.schemes[0].false_stale, 0,
            "content_only should have zero false_stale with correct ground truth"
        );
    }

    #[test]
    fn chunk_has_nonzero_false_stale() {
        // Candidate B reports Stale for unchanged lines when a nearby line
        // in the same chunk changed (chunk context invalidation). These are
        // false_stale: the line is unchanged but the scheme conservatively
        // rejects it.
        let corpus = test_corpus();
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![16],
            checkpoint_intervals: vec![],
            search_radius: DEFAULT_SEARCH_RADIUS,
        };
        let report = run_benchmark(&corpus, &config);
        assert_eq!(report.schemes.len(), 2); // A + B
        let b = &report.schemes[1];
        assert!(
            b.false_stale > 0,
            "chunk scheme should have some false_stale from chunk invalidation"
        );
    }

    #[test]
    fn chunk_has_higher_stale_recall_than_content_only() {
        // Candidate B should detect more staleness than A because it also
        // invalidates when the chunk changes.
        let corpus = test_corpus();
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![16],
            checkpoint_intervals: vec![],
            search_radius: DEFAULT_SEARCH_RADIUS,
        };
        let report = run_benchmark(&corpus, &config);
        assert_eq!(report.schemes.len(), 2); // A + B

        let a = &report.schemes[0];
        let b = &report.schemes[1];

        // B should report at least as many stale results as A.
        let a_stale = a.true_stale + a.false_stale;
        let b_stale = b.true_stale + b.false_stale;
        assert!(
            b_stale >= a_stale,
            "chunk should detect at least as many stale as content_only: B={b_stale} A={a_stale}"
        );
    }

    #[test]
    fn repetitive_file_shows_collisions() {
        let corpus = vec![("repetitive.rs", REPETITIVE_FILE)];
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![],
            checkpoint_intervals: vec![],
            search_radius: DEFAULT_SEARCH_RADIUS,
        };
        let report = run_benchmark(&corpus, &config);
        // Repetitive file has identical struct fields ("name: String," etc.)
        // so content-only should show collisions.
        assert!(
            report.schemes[0].collision_count > 0,
            "repetitive file should produce collisions"
        );
    }

    #[test]
    fn edit_trace_metrics_populated() {
        let corpus = test_corpus();
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![16],
            checkpoint_intervals: vec![],
            search_radius: DEFAULT_SEARCH_RADIUS,
        };
        let report = run_benchmark(&corpus, &config);

        for m in &report.schemes {
            assert!(
                m.trace_steps > 0,
                "trace_steps should be > 0 for {}",
                m.label
            );
        }
    }

    #[test]
    fn report_display_does_not_panic() {
        let corpus = test_corpus();
        let report = run_benchmark(&corpus, &BenchmarkConfig::default());
        let output = format!("{report}");
        assert!(output.contains("Hashline Anchor Benchmark Report"));
        assert!(output.contains("content_only"));
        assert!(output.contains("chunk"));
        assert!(output.contains("checkpoint"));
    }

    #[test]
    fn stale_precision_recall_bounds() {
        let corpus = test_corpus();
        let report = run_benchmark(&corpus, &BenchmarkConfig::default());
        for m in &report.schemes {
            let p = m.stale_precision();
            let r = m.stale_recall();
            assert!(
                (0.0..=1.0).contains(&p),
                "precision out of bounds for {}: {p}",
                m.label
            );
            assert!(
                (0.0..=1.0).contains(&r),
                "recall out of bounds for {}: {r}",
                m.label
            );
        }
    }

    #[test]
    fn empty_corpus_produces_empty_report() {
        let report = run_benchmark(&[], &BenchmarkConfig::default());
        assert_eq!(report.corpus_files, 0);
        assert_eq!(report.total_corpus_lines, 0);
        for m in &report.schemes {
            assert_eq!(m.total_validations, 0);
        }
    }

    #[test]
    fn single_scheme_config() {
        let corpus = test_corpus();
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![],
            checkpoint_intervals: vec![],
            search_radius: 5,
        };
        let report = run_benchmark(&corpus, &config);
        assert_eq!(report.schemes.len(), 1);
        assert!(report.schemes[0].label.contains("content_only"));
    }

    #[test]
    fn read_amplification_content_only_is_one() {
        let corpus = test_corpus();
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![],
            checkpoint_intervals: vec![],
            search_radius: DEFAULT_SEARCH_RADIUS,
        };
        let report = run_benchmark(&corpus, &config);
        let a = &report.schemes[0];
        // Content-only reads 1 line per validation → avg should be 1.0.
        let avg = a.avg_read_amp_lines();
        assert!(
            (avg - 1.0).abs() < f64::EPSILON,
            "content_only avg read amp should be 1.0, got {avg}"
        );
    }

    #[test]
    fn read_amplification_chunk_higher_than_content_only() {
        let corpus = test_corpus();
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![16],
            checkpoint_intervals: vec![],
            search_radius: DEFAULT_SEARCH_RADIUS,
        };
        let report = run_benchmark(&corpus, &config);
        let a = &report.schemes[0];
        let b = &report.schemes[1];
        assert!(
            b.avg_read_amp_lines() > a.avg_read_amp_lines(),
            "chunk read amp ({}) should be > content_only ({})",
            b.avg_read_amp_lines(),
            a.avg_read_amp_lines()
        );
    }

    #[test]
    fn recovery_correctness_tracked() {
        // Verify that recovery_correct + recovery_wrong + recovery_ambiguous
        // + recovery_not_found == recovery_attempts.
        let corpus = test_corpus();
        let config = BenchmarkConfig {
            hash_lengths: vec![3],
            chunk_sizes: vec![],
            checkpoint_intervals: vec![],
            search_radius: DEFAULT_SEARCH_RADIUS,
        };
        let report = run_benchmark(&corpus, &config);
        for m in &report.schemes {
            let total =
                m.recovery_correct + m.recovery_wrong + m.recovery_ambiguous + m.recovery_not_found;
            assert_eq!(
                total, m.recovery_attempts,
                "recovery outcomes should sum to attempts for {}",
                m.label
            );
        }
    }
}
