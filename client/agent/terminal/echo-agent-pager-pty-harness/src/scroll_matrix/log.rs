//! `ECHO_AGENT_SCROLL_LOG` JSONL parsing, per-stream grouping, and finalize
//! synchronization.
//!
//! Wire schema source of truth: the pager's `ScrollLogRecord` in
//! `echo-agent-pager/src/input/scroll_log.rs`. [`ScrollLogLine`] mirrors it
//! field-for-field with every always-emitted field **required** (see the
//! module docs in [`super`] for the drift-tripwire rationale).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// `evt` value of a stream-start record (config echo rides these only).
pub const EVT_STREAM_START: &str = "stream_start";
/// `evt` value of a mid-stream line-delivering flush record.
pub const EVT_FLUSH: &str = "flush";
/// `evt` value of a stream-finalize record (`dropped` rides these only).
pub const EVT_FINALIZE: &str = "finalize";

/// One parsed flight-recorder line.
///
/// Field names and required/optional split mirror the producer's
/// `ScrollLogRecord` (`scroll_log.rs`): the producer always emits the
/// non-`Option` fields, `#[serde(skip_serializing_if)]`s the `Option`
/// bookkeeping fields, and `#[serde(flatten)]`s the config echo onto
/// `stream_start` records only. Unknown fields are tolerated (additive
/// producer changes must not break older matrix code); missing required
/// fields fail loudly with the line number via [`parse_jsonl`].
#[derive(Debug, Clone, Deserialize)]
pub struct ScrollLogLine {
    /// Monotonic ms since recorder start (the state machine's timeline).
    pub ts_ms: f64,
    /// Record type: [`EVT_STREAM_START`] | [`EVT_FLUSH`] | [`EVT_FINALIZE`].
    pub evt: String,
    /// Emitting code path: `event` | `tick` | `promotion` | `finalize`.
    pub trigger: String,
    /// Stream classification (`unknown` until promotion/finalize).
    pub kind: String,
    /// Events accumulated in the stream so far.
    pub events_total: u64,
    /// Arrivals since the last logged flush/finalize of this stream.
    pub events_since_flush: u64,
    /// Acceleration multiplier in effect.
    pub accel: f32,
    /// Fractional target lines (post accel/speed multipliers, carry included).
    pub desired: f32,
    /// Whole lines delivered for this stream so far (post-flush).
    pub applied_total: i64,
    /// Lines this record's flush delivered (0 on stream_start).
    pub flushed: i64,
    /// Whole-line backlog remaining after this record's flush.
    pub backlog_after: i64,
    /// Sub-line remainder included in `desired`.
    pub carry: f32,
    /// Per-flush delta cap in effect.
    pub cap: i64,

    /// Rolling average inter-event interval (ms); absent until two
    /// accel-countable events arrived.
    pub avg_interval_ms: Option<f64>,
    /// Spacing from the previous flush-bearing record; absent before the
    /// first. **Global, not per-stream** — the producer's `last_flush_at`
    /// never resets at stream boundaries, so on a stream's first
    /// flush-bearing record this measures from the *previous stream*. Use
    /// [`StreamGroup::intra_stream_flush_spacings_ms`] for per-stream
    /// cadence.
    pub ms_since_prev_flush: Option<f64>,
    /// Finalize only: whole lines discarded with the stream.
    pub dropped: Option<i64>,

    // Config echo, flattened onto stream_start records only.
    /// Scroll input mode label in effect (`auto` | `wheel` | `trackpad`).
    pub mode: Option<String>,
    /// Events per tick.
    pub ept: Option<u16>,
    /// Wheel lines per tick.
    pub wheel_lpt: Option<u16>,
    /// Trackpad lines per tick.
    pub trackpad_lpt: Option<u16>,
    /// Direction inversion.
    pub invert: Option<bool>,
    /// Speed multiplier.
    pub speed: Option<f32>,
    /// Viewport height stamped on the config.
    pub viewport_height: Option<u16>,
}

impl ScrollLogLine {
    /// Whether this is a stream-start record.
    pub fn is_stream_start(&self) -> bool {
        self.evt == EVT_STREAM_START
    }

    /// Whether this is a mid-stream flush record.
    pub fn is_flush(&self) -> bool {
        self.evt == EVT_FLUSH
    }

    /// Whether this is a finalize record.
    pub fn is_finalize(&self) -> bool {
        self.evt == EVT_FINALIZE
    }
}

/// Parse a `ECHO_AGENT_SCROLL_LOG` JSONL file into records.
///
/// Every line must parse — errors carry the 1-based line number and the
/// offending line. Call after the capture is quiescent (the producer
/// force-flushes on finalize, so a file whose last gesture finalized ends
/// on a record boundary); mid-write reads can see a torn tail line, which
/// fails here by design — synchronize with [`wait_for_finalize_count`]
/// first.
pub fn parse_jsonl(path: &Path) -> Result<Vec<ScrollLogLine>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read scroll log {}", path.display()))?;
    parse_jsonl_str(&raw).with_context(|| format!("scroll log {}", path.display()))
}

/// [`parse_jsonl`] over an in-memory JSONL string (fixtures, pre-read files).
pub fn parse_jsonl_str(raw: &str) -> Result<Vec<ScrollLogLine>> {
    raw.lines()
        .enumerate()
        .map(|(idx, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("line {}: failed to parse {line:?}", idx + 1))
        })
        .collect()
}

/// One recorded gesture: `stream_start` → `flush`* → `finalize`.
///
/// `finalize` is `None` only for a trailing stream still in flight when the
/// capture ended: finalize rides the 80ms-gap and direction-flip
/// transitions, so a capture cut mid-gesture leaves the last stream open.
#[derive(Debug, Clone)]
pub struct StreamGroup<'a> {
    /// The `stream_start` record (carries the config echo).
    pub start: &'a ScrollLogLine,
    /// Mid-stream `flush` records, in file order.
    pub flushes: Vec<&'a ScrollLogLine>,
    /// The `finalize` record; `None` for a trailing in-flight stream.
    pub finalize: Option<&'a ScrollLogLine>,
}

impl<'a> StreamGroup<'a> {
    /// Whether the stream's finalize record was captured.
    pub fn is_finalized(&self) -> bool {
        self.finalize.is_some()
    }

    /// Flush-bearing records (flushes, then finalize if present), in order.
    pub fn flush_bearing(&self) -> impl Iterator<Item = &'a ScrollLogLine> + '_ {
        self.flushes.iter().copied().chain(self.finalize)
    }

    /// Intra-stream flush spacings (ms), in order.
    ///
    /// Skips the stream's **first** flush-bearing record: the producer's
    /// `ms_since_prev_flush` is global (its `last_flush_at` never resets at
    /// stream boundaries — see `scroll_log.rs`), so the first value
    /// measures from the previous stream's last flush/finalize and says
    /// nothing about this stream's cadence. Every subsequent flush-bearing
    /// record's spacing is intra-stream by construction.
    pub fn intra_stream_flush_spacings_ms(&self) -> Vec<f64> {
        self.flush_bearing()
            .skip(1)
            .filter_map(|record| record.ms_since_prev_flush)
            .collect()
    }
}

/// Group parsed records into per-gesture [`StreamGroup`]s.
///
/// Expects the shape the pager emits when the recorder exists for the whole
/// session (`ECHO_AGENT_SCROLL_LOG` set at spawn): `stream_start` → `flush`* →
/// `finalize`, repeated, with at most one trailing unfinalized stream. A
/// direction flip emits `finalize` and the next `stream_start` at the same
/// `ts_ms`; that boundary is a plain group boundary here.
///
/// Malformed shapes (flush/finalize before any start, start while a stream
/// is open) are errors: with a spawn-time recorder they indicate producer
/// drift. Caveat: a `/debug log` *runtime-toggled* recorder can begin
/// mid-stream and legitimately open with an orphan flush/finalize — the
/// matrix never does that, so it is rejected rather than silently grouped.
pub fn group_streams(records: &[ScrollLogLine]) -> Result<Vec<StreamGroup<'_>>> {
    let mut groups: Vec<StreamGroup<'_>> = Vec::new();
    let mut open: Option<StreamGroup<'_>> = None;

    for (idx, record) in records.iter().enumerate() {
        let record_no = idx + 1;
        match record.evt.as_str() {
            EVT_STREAM_START => {
                if open.is_some() {
                    bail!(
                        "record {record_no}: stream_start at ts_ms={} while the previous \
                         stream is still open (producer emits finalize before the next start)",
                        record.ts_ms
                    );
                }
                open = Some(StreamGroup {
                    start: record,
                    flushes: Vec::new(),
                    finalize: None,
                });
            }
            EVT_FLUSH => match open.as_mut() {
                Some(group) => group.flushes.push(record),
                None => bail!(
                    "record {record_no}: flush at ts_ms={} with no open stream \
                     (missing stream_start)",
                    record.ts_ms
                ),
            },
            EVT_FINALIZE => match open.take() {
                Some(mut group) => {
                    group.finalize = Some(record);
                    groups.push(group);
                }
                None => bail!(
                    "record {record_no}: finalize at ts_ms={} with no open stream \
                     (missing stream_start)",
                    record.ts_ms
                ),
            },
            other => bail!("record {record_no}: unknown evt {other:?}"),
        }
    }

    // A trailing stream still in flight at capture end is legitimate.
    groups.extend(open);
    Ok(groups)
}

/// Poll interval for [`wait_for_finalize_count`]. Short enough that the
/// wait adds at most ~10ms latency past the write, long enough not to spin.
const FINALIZE_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Block until `path` contains at least `n` finalize records, or `timeout`
/// expires.
///
/// The producer force-flushes its `BufWriter` on every finalize record
/// (gesture boundary — the `tail -f` contract in `scroll_log.rs`), so
/// polling the file is race-free for finalize counting: once the flush
/// lands, the line is fully present. Counting uses a raw substring match
/// (`"evt":"finalize"` — serde_json's compact encoding) rather than a full
/// parse so a torn non-finalize tail mid-write can't fail the wait. A
/// not-yet-created file (the recorder opens lazily on the first record)
/// counts as zero.
pub fn wait_for_finalize_count(path: &Path, n: usize, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let count = count_finalize_lines(path)?;
        if count >= n {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out after {timeout:?} waiting for {n} finalize record(s) in {}: found {count}",
                path.display()
            );
        }
        std::thread::sleep(FINALIZE_POLL_INTERVAL);
    }
}

/// Count finalize records by raw substring; missing file counts as zero.
fn count_finalize_lines(path: &Path) -> Result<usize> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(raw.matches("\"evt\":\"finalize\"").count()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => {
            Err(err).with_context(|| format!("failed to read scroll log {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixture lines shaped exactly like the producer's serde output
    // (compact JSON, snake_case evt/trigger, config echo flattened onto
    // stream_start, skip-if-None optionals) — copied from the wire format
    // pinned by `scroll_log_records_flood_flushes_and_capped_finalize_drop`
    // in `echo-agent-pager/src/input/mouse/tests.rs`.
    const START: &str = r#"{"ts_ms":0.0,"evt":"stream_start","trigger":"event","kind":"unknown","events_total":0,"events_since_flush":0,"accel":1.0,"desired":0.0,"applied_total":0,"flushed":0,"backlog_after":0,"carry":0.0,"cap":6,"mode":"trackpad","ept":3,"wheel_lpt":3,"trackpad_lpt":3,"invert":false,"speed":1.0,"viewport_height":40}"#;
    const FLUSH_FIRST: &str = r#"{"ts_ms":16.0,"evt":"flush","trigger":"event","kind":"trackpad","events_total":9,"events_since_flush":9,"avg_interval_ms":2.0,"accel":1.0,"desired":9.4,"applied_total":6,"flushed":6,"backlog_after":3,"carry":0.4,"cap":6}"#;
    const FLUSH_SECOND: &str = r#"{"ts_ms":32.0,"evt":"flush","trigger":"tick","kind":"trackpad","events_total":17,"events_since_flush":8,"avg_interval_ms":2.0,"accel":1.0,"desired":17.4,"applied_total":12,"flushed":6,"backlog_after":5,"carry":0.4,"cap":6,"ms_since_prev_flush":16.0}"#;
    const FINALIZE: &str = r#"{"ts_ms":114.0,"evt":"finalize","trigger":"finalize","kind":"trackpad","events_total":50,"events_since_flush":33,"avg_interval_ms":2.0,"accel":1.0,"desired":50.0,"applied_total":18,"flushed":6,"backlog_after":26,"carry":0.0,"cap":6,"ms_since_prev_flush":82.0,"dropped":26}"#;

    fn jsonl(lines: &[&str]) -> String {
        let mut out = lines.join("\n");
        out.push('\n'); // producer's writeln! always terminates lines
        out
    }

    #[test]
    fn parses_producer_shaped_fixture_lines() {
        let records =
            parse_jsonl_str(&jsonl(&[START, FLUSH_FIRST, FLUSH_SECOND, FINALIZE])).expect("parse");
        assert_eq!(records.len(), 4);

        let start = &records[0];
        assert!(start.is_stream_start());
        assert_eq!(start.trigger, "event");
        assert_eq!(start.kind, "unknown");
        assert_eq!(start.events_total, 0);
        assert_eq!(start.cap, 6);
        // Config echo rides stream_start only.
        assert_eq!(start.mode.as_deref(), Some("trackpad"));
        assert_eq!(start.ept, Some(3));
        assert_eq!(start.wheel_lpt, Some(3));
        assert_eq!(start.trackpad_lpt, Some(3));
        assert_eq!(start.invert, Some(false));
        assert_eq!(start.speed, Some(1.0));
        assert_eq!(start.viewport_height, Some(40));
        assert!(start.dropped.is_none());
        assert!(start.ms_since_prev_flush.is_none());

        let first_flush = &records[1];
        assert!(first_flush.is_flush());
        assert_eq!(first_flush.flushed, 6);
        assert_eq!(first_flush.avg_interval_ms, Some(2.0));
        // No flush-bearing record precedes it in this capture.
        assert!(first_flush.ms_since_prev_flush.is_none());
        assert!(first_flush.mode.is_none(), "flushes skip the config echo");

        let finalize = &records[3];
        assert!(finalize.is_finalize());
        assert_eq!(finalize.trigger, "finalize");
        assert_eq!(finalize.dropped, Some(26));
        assert_eq!(finalize.backlog_after, 26);
        assert_eq!(finalize.ms_since_prev_flush, Some(82.0));
        assert_eq!(finalize.ts_ms, 114.0);
    }

    #[test]
    fn unknown_fields_are_tolerated_for_additive_producer_changes() {
        let with_future_field = FLUSH_SECOND.replace(
            "\"ms_since_prev_flush\":16.0",
            "\"ms_since_prev_flush\":16.0,\"future_field\":true",
        );
        let records = parse_jsonl_str(&jsonl(&[START, &with_future_field]))
            .expect("additive fields must not break ingestion");
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn missing_required_field_fails_with_line_number() {
        // Simulate a producer-side rename: `carry` disappears from line 2.
        let renamed = FLUSH_FIRST.replace("\"carry\":0.4,", "");
        let err = parse_jsonl_str(&jsonl(&[START, &renamed])).expect_err("drift must fail");
        let chain = format!("{err:#}");
        assert!(chain.contains("line 2"), "no line number in: {chain}");
        assert!(chain.contains("carry"), "no missing-field name in: {chain}");
    }

    #[test]
    fn groups_flip_boundary_and_trailing_unfinalized_stream() {
        // Direction flip: finalize and the next stream_start share ts_ms
        // (both emitted from the same on_scroll_event call). The capture
        // ends with stream 2 still in flight (no finalize).
        let flip_finalize = FINALIZE.replace("\"ts_ms\":114.0", "\"ts_ms\":40.0");
        let flip_start = START.replace("\"ts_ms\":0.0", "\"ts_ms\":40.0");
        let records = parse_jsonl_str(&jsonl(&[
            START,
            FLUSH_FIRST,
            FLUSH_SECOND,
            &flip_finalize,
            &flip_start,
            &FLUSH_SECOND.replace("\"ts_ms\":32.0", "\"ts_ms\":56.0"),
        ]))
        .expect("parse");

        let groups = group_streams(&records).expect("well-formed grouping");
        assert_eq!(groups.len(), 2);

        let first = &groups[0];
        assert!(first.is_finalized());
        assert_eq!(first.flushes.len(), 2);
        assert_eq!(first.finalize.expect("finalized").ts_ms, 40.0);

        let trailing = &groups[1];
        assert!(!trailing.is_finalized(), "in-flight at capture end");
        assert_eq!(trailing.start.ts_ms, 40.0, "flip start shares finalize ts");
        assert_eq!(trailing.flushes.len(), 1);
    }

    #[test]
    fn orphan_records_and_double_start_are_rejected() {
        let records = parse_jsonl_str(&jsonl(&[FLUSH_FIRST])).expect("parse");
        let err = group_streams(&records).expect_err("orphan flush");
        assert!(format!("{err:#}").contains("no open stream"));

        let records = parse_jsonl_str(&jsonl(&[FINALIZE])).expect("parse");
        let err = group_streams(&records).expect_err("orphan finalize");
        assert!(format!("{err:#}").contains("no open stream"));

        let records = parse_jsonl_str(&jsonl(&[START, FLUSH_FIRST, START])).expect("parse");
        let err = group_streams(&records).expect_err("start while open");
        assert!(format!("{err:#}").contains("still open"));
    }

    #[test]
    fn per_stream_spacing_skips_the_global_first_flush_record() {
        // Stream 2's first flush carries ms_since_prev_flush measured from
        // stream 1's finalize (producer's last_flush_at is global) — 500ms
        // of inter-gesture idle that is NOT stream-2 cadence and must be
        // skipped; the later flush (16ms) and finalize (82ms) are kept.
        let s2_start = START.replace("\"ts_ms\":0.0", "\"ts_ms\":600.0");
        let s2_flush_global = FLUSH_SECOND
            .replace("\"ts_ms\":32.0", "\"ts_ms\":616.0")
            .replace(
                "\"ms_since_prev_flush\":16.0",
                "\"ms_since_prev_flush\":500.0",
            );
        let s2_flush_intra = FLUSH_SECOND.replace("\"ts_ms\":32.0", "\"ts_ms\":632.0");
        let s2_finalize = FINALIZE.replace("\"ts_ms\":114.0", "\"ts_ms\":714.0");
        let records = parse_jsonl_str(&jsonl(&[
            START,
            FLUSH_FIRST,
            FINALIZE,
            &s2_start,
            &s2_flush_global,
            &s2_flush_intra,
            &s2_finalize,
        ]))
        .expect("parse");

        let groups = group_streams(&records).expect("grouping");
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[1].intra_stream_flush_spacings_ms(),
            vec![16.0, 82.0],
            "the 500ms cross-stream value must be skipped"
        );
        // Stream 1: first flush has no spacing at all (recorder start);
        // only the finalize's intra-stream spacing remains.
        assert_eq!(groups[0].intra_stream_flush_spacings_ms(), vec![82.0]);
    }

    #[test]
    fn wait_for_finalize_count_write_then_check_phases() {
        use std::io::Write;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("scroll-log.jsonl");

        // Lazy-open recorder: the file does not exist yet — n=0 succeeds.
        wait_for_finalize_count(&path, 0, Duration::ZERO).expect("n=0 on missing file");
        // …but n=1 times out, reporting the current count.
        let err = wait_for_finalize_count(&path, 1, Duration::from_millis(30))
            .expect_err("missing file cannot satisfy n=1");
        assert!(format!("{err:#}").contains("found 0"), "err: {err:#}");

        let mut file = std::fs::File::create(&path).expect("create");
        writeln!(file, "{START}").expect("write");
        writeln!(file, "{FLUSH_FIRST}").expect("write");
        writeln!(file, "{FINALIZE}").expect("write");
        file.flush().expect("flush");
        wait_for_finalize_count(&path, 1, Duration::from_secs(5)).expect("one finalize present");

        // A torn tail mid-write (BufWriter fill boundary) must not fail the
        // wait — counting is a raw substring match, not a parse.
        write!(file, "{{\"ts_ms\":9").expect("torn tail");
        file.flush().expect("flush");
        wait_for_finalize_count(&path, 1, Duration::from_secs(5)).expect("torn tail tolerated");
        let err = wait_for_finalize_count(&path, 2, Duration::from_millis(30))
            .expect_err("only one finalize so far");
        assert!(format!("{err:#}").contains("found 1"), "err: {err:#}");

        // Completing the torn record into a second finalize satisfies n=2.
        writeln!(file, ".0,\"evt\":\"finalize\"}}").expect("complete tail");
        file.flush().expect("flush");
        wait_for_finalize_count(&path, 2, Duration::from_secs(5)).expect("two finalizes");
    }

    #[test]
    fn wait_for_finalize_count_observes_concurrent_appends() {
        use std::io::Write;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("scroll-log.jsonl");
        let writer_path = path.clone();

        // Bounded writer delay, generous deadline: the wait must return as
        // soon as the poll sees the flushed finalize, not at the deadline.
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let mut file = std::fs::File::create(&writer_path).expect("create");
            writeln!(file, "{START}").expect("write");
            writeln!(file, "{FINALIZE}").expect("write");
            file.flush().expect("flush");
        });

        wait_for_finalize_count(&path, 1, Duration::from_secs(10))
            .expect("poll must observe the concurrent append");
        writer.join().expect("writer thread");
    }
}
