//! Repeated warm-process benchmark for shell session listing.
//!
//! The shell-core case measures `build_unified_list` after request parsing
//! through local row construction. It excludes response serialization, ACP
//! transport, and pager parsing, filtering, and rendering. Storage cases are
//! diagnostics.
//!
//! The fixture has 3,000 encoded workspaces and 9,864 summaries. Its 32
//! same-repo CWDs are the main checkout, 15 DB/filesystem overlaps, one dead
//! DB-only worktree, and 15 filesystem-only worktrees, all with current labels
//! and interleaved activity. Another 2,968 unrelated CWDs provide scale.
//!
//! Setup and exact assertions are outside timing. Samples reuse the tree and
//! process, so filesystem and JSON work uses a warm OS page cache. Fixed
//! year-2100 timestamps pass the pager cutoff, though pager stages are excluded.
//!
//! Run: `cargo bench -p echo-agent-runtime --bench session_list`
//! Allow roughly 4-8 minutes after compilation for the configured samples.

use std::collections::HashSet;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_client_protocol as acp;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use echo_agent_fast_worktree::{
    ListFilter, WorktreeDb, WorktreeKind, WorktreeRecord, WorktreeStatus,
};
use echo_agent_runtime::session::info::Info;
use echo_agent_runtime::session::persistence::Summary;
use echo_agent_runtime::session::storage::{JsonlStorageAdapter, StorageAdapter};
use echo_agent_runtime::session::unified_list::{ListReq, UnifiedListResult, build_unified_list};
use filetime::{FileTime, set_file_mtime};
use tempfile::TempDir;

const WORKSPACE_COUNT: usize = 3_000;
// Bump whenever workload semantics change, even if aggregate counts do not.
const FIXTURE_SCHEMA_VERSION: usize = 1;
const MAIN_CHECKOUT_COUNT: usize = 1;
const LINKED_WORKTREE_COUNT: usize = 30;
const DB_OVERLAP_WORKTREE_COUNT: usize = 15;
const DB_ONLY_WORKTREE_COUNT: usize = 1;
const FILESYSTEM_ONLY_WORKTREE_COUNT: usize = LINKED_WORKTREE_COUNT - DB_OVERLAP_WORKTREE_COUNT;
const DB_TRACKED_WORKTREE_COUNT: usize = DB_OVERLAP_WORKTREE_COUNT + DB_ONLY_WORKTREE_COUNT;
const SAME_REPO_CANDIDATE_COUNT: usize =
    MAIN_CHECKOUT_COUNT + LINKED_WORKTREE_COUNT + DB_ONLY_WORKTREE_COUNT;
const SESSIONS_PER_SAME_REPO_CWD: usize = 30;
const SESSIONS_PER_UNRELATED_WORKSPACE: usize = 3;
const SAME_REPO_SUMMARY_COUNT: usize = SAME_REPO_CANDIDATE_COUNT * SESSIONS_PER_SAME_REPO_CWD;
const TOTAL_SUMMARY_COUNT: usize = SAME_REPO_SUMMARY_COUNT
    + (WORKSPACE_COUNT - SAME_REPO_CANDIDATE_COUNT) * SESSIONS_PER_UNRELATED_WORKSPACE;
const RECENT_LIMIT: usize = 30;
const SAMPLE_SIZE: usize = 10;
const ACTIVITY_ROTATION: usize = 2;
const COOPERATIVE_PEER_DELAY: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateSource {
    Main,
    Overlap,
    DbOnly,
    FilesystemOnly,
}

#[derive(Clone)]
struct ExpectedSession {
    active_at: DateTime<Utc>,
    id: String,
    workspace_index: usize,
}

struct Fixture {
    _home: TempDir,
    adapter: JsonlStorageAdapter,
    picker_cwd: String,
    same_repo_cwds: Vec<String>,
    same_repo_labels: Vec<Option<String>>,
    same_repo_sources: Vec<CandidateSource>,
    all_ids_desc: Vec<String>,
    picker_ids_desc: Vec<String>,
    same_repo_ids_desc: Vec<String>,
    unified_sources: Vec<CandidateSource>,
    recent_ids_desc: Vec<String>,
}

impl Fixture {
    fn new(home: TempDir) -> Self {
        let sessions_root = home.path().join("sessions");
        fs::create_dir(&sessions_root).expect("create sessions root");
        let topology = create_same_repo_cwds(home.path());

        let base = DateTime::parse_from_rfc3339("2100-01-01T00:00:00Z")
            .expect("valid fixture timestamp")
            .with_timezone(&Utc);
        let picker_cwd = topology.cwds[0].clone();
        let mut all_sessions = Vec::with_capacity(TOTAL_SUMMARY_COUNT);
        let mut picker_sessions = Vec::with_capacity(SESSIONS_PER_SAME_REPO_CWD);
        let mut same_repo_sessions = Vec::with_capacity(SAME_REPO_SUMMARY_COUNT);

        for workspace_index in 0..WORKSPACE_COUNT {
            let same_repo = workspace_index < SAME_REPO_CANDIDATE_COUNT;
            let cwd = if same_repo {
                topology.cwds[workspace_index].clone()
            } else {
                benchmark_unrelated_cwd(workspace_index - SAME_REPO_CANDIDATE_COUNT)
            };
            let encoded = echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd);
            let cwd_dir = sessions_root.join(encoded);
            fs::create_dir(&cwd_dir).expect("create encoded cwd directory");
            let session_count = if same_repo {
                SESSIONS_PER_SAME_REPO_CWD
            } else {
                SESSIONS_PER_UNRELATED_WORKSPACE
            };

            for session_index in 0..session_count {
                let ordinal = all_sessions.len();
                let session_id = format!("bench-session-{workspace_index:04}-{session_index:02}");
                let activity_offset = if same_repo {
                    same_repo_activity_offset(workspace_index, session_index)
                } else {
                    SAME_REPO_SUMMARY_COUNT
                        + (workspace_index - SAME_REPO_CANDIDATE_COUNT)
                            * SESSIONS_PER_UNRELATED_WORKSPACE
                        + session_index
                };
                let active_at = base + ChronoDuration::seconds(activity_offset as i64);
                let worktree_label = topology
                    .labels
                    .get(workspace_index)
                    .and_then(Option::as_deref);
                write_summary(
                    &cwd_dir,
                    &cwd,
                    &session_id,
                    active_at,
                    ordinal,
                    worktree_label,
                );
                let expected = ExpectedSession {
                    active_at,
                    id: session_id,
                    workspace_index,
                };
                if workspace_index == 0 {
                    picker_sessions.push(expected.clone());
                }
                if same_repo {
                    same_repo_sessions.push(expected.clone());
                }
                all_sessions.push(expected);
            }
        }

        sort_expected_sessions(&mut all_sessions);
        sort_expected_sessions(&mut picker_sessions);
        sort_expected_sessions(&mut same_repo_sessions);
        let all_ids: Vec<_> = all_sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        let picker_ids: Vec<String> = picker_sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        let same_repo_ids: Vec<String> = same_repo_sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        let unified_sources: Vec<CandidateSource> = same_repo_sessions
            .iter()
            .take(RECENT_LIMIT)
            .map(|session| topology.sources[session.workspace_index])
            .collect();
        let recent_ids = all_ids.iter().take(RECENT_LIMIT).cloned().collect();
        assert_eq!(all_ids.len(), TOTAL_SUMMARY_COUNT);
        assert_eq!(same_repo_ids.len(), SAME_REPO_SUMMARY_COUNT);

        let adapter = JsonlStorageAdapter::with_root(home.path().to_path_buf());
        Self {
            _home: home,
            adapter,
            picker_cwd,
            same_repo_cwds: topology.cwds,
            same_repo_labels: topology.labels,
            same_repo_sources: topology.sources,
            all_ids_desc: all_ids,
            picker_ids_desc: picker_ids,
            same_repo_ids_desc: same_repo_ids,
            unified_sources,
            recent_ids_desc: recent_ids,
        }
    }

    fn assert_correct(&self, runtime: &tokio::runtime::Runtime) {
        let discovered =
            echo_agent_runtime::session::worktree::candidate_worktree_cwds_for_same_repo(
                Path::new(&self.picker_cwd),
            )
            .expect("discover same-repo candidate cwd paths");
        assert_eq!(discovered, self.same_repo_cwds);

        for (index, (cwd, expected_label)) in self
            .same_repo_cwds
            .iter()
            .zip(&self.same_repo_labels)
            .enumerate()
        {
            let summaries = runtime
                .block_on(self.adapter.list_sessions(Some(cwd)))
                .expect("list same-repo cwd sessions");
            assert_eq!(summaries.len(), SESSIONS_PER_SAME_REPO_CWD);
            assert!(
                summaries
                    .iter()
                    .all(|summary| summary.worktree_label.as_deref() == expected_label.as_deref())
            );
            if index == 0 {
                assert_eq!(summary_ids(&summaries), self.picker_ids_desc);
            }
        }

        let all = runtime
            .block_on(self.adapter.list_sessions(None))
            .expect("list all sessions");
        assert_eq!(summary_ids(&all), self.all_ids_desc);

        let recent = runtime
            .block_on(self.adapter.list_sessions_recent(RECENT_LIMIT))
            .expect("list recent sessions");
        assert_eq!(recent.len(), RECENT_LIMIT);
        assert_eq!(summary_ids(&recent), self.recent_ids_desc);

        let unified = runtime.block_on(build_local_list_with_delayed_peer(self.picker_cwd.clone()));
        let unified_ids: Vec<_> = unified
            .rows
            .iter()
            .map(|row| row.legacy.session_id.clone())
            .collect();
        assert_eq!(
            unified_ids.as_slice(),
            &self.same_repo_ids_desc[..RECENT_LIMIT]
        );
        let mut actual_sources = Vec::with_capacity(RECENT_LIMIT);
        let mut top_cwds = HashSet::with_capacity(RECENT_LIMIT);
        for row in &unified.rows {
            let workspace_index = self
                .same_repo_cwds
                .iter()
                .position(|cwd| cwd == &row.legacy.cwd)
                .expect("unified row cwd belongs to same-repo topology");
            actual_sources.push(self.same_repo_sources[workspace_index]);
            top_cwds.insert(row.legacy.cwd.as_str());
            assert_eq!(
                row.legacy.worktree_label.as_deref(),
                self.same_repo_labels[workspace_index].as_deref()
            );
        }
        assert_eq!(actual_sources, self.unified_sources);
        assert_eq!(top_cwds.len(), RECENT_LIMIT);
        let source_count = |source| {
            actual_sources
                .iter()
                .filter(|candidate| **candidate == source)
                .count()
        };
        assert_eq!(source_count(CandidateSource::Main), MAIN_CHECKOUT_COUNT);
        assert_eq!(
            source_count(CandidateSource::Overlap),
            DB_OVERLAP_WORKTREE_COUNT
        );
        assert_eq!(
            source_count(CandidateSource::DbOnly),
            DB_ONLY_WORKTREE_COUNT
        );
        assert_eq!(
            source_count(CandidateSource::FilesystemOnly),
            RECENT_LIMIT - MAIN_CHECKOUT_COUNT - DB_OVERLAP_WORKTREE_COUNT - DB_ONLY_WORKTREE_COUNT
        );
    }
}

struct SameRepoTopology {
    cwds: Vec<String>,
    labels: Vec<Option<String>>,
    sources: Vec<CandidateSource>,
}

fn create_same_repo_cwds(home: &Path) -> SameRepoTopology {
    let repo_dir = home.join("fixture").join("main");
    fs::create_dir_all(&repo_dir).expect("create main checkout directory");
    let repo = git2::Repository::init(&repo_dir).expect("initialize main git repository");
    repo.remote(
        "origin",
        "https://github.com/example/session-list-benchmark.git",
    )
    .expect("create benchmark git remote");

    let signature = git2::Signature::new(
        "session-list-benchmark",
        "benchmark@localhost",
        &git2::Time::new(1_700_000_000, 0),
    )
    .expect("create git signature");
    let tree_id = repo
        .index()
        .expect("open git index")
        .write_tree()
        .expect("write empty git tree");
    let tree = repo.find_tree(tree_id).expect("find empty git tree");
    let commit_id = repo
        .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .expect("create initial git commit");

    let main = dunce::canonicalize(&repo_dir).expect("canonicalize main checkout");
    let worktree_base = echo_agent_runtime::session::worktree::worktree_base_dir(&main);
    fs::create_dir_all(&worktree_base).expect("create worktree base");
    let canonical_worktree_base =
        dunce::canonicalize(&worktree_base).expect("canonicalize worktree base");
    let mut linked = Vec::with_capacity(LINKED_WORKTREE_COUNT);
    for index in 1..=LINKED_WORKTREE_COUNT {
        let name = format!("session-list-{index:02}");
        let path = worktree_base.join(&name);
        repo.worktree(&name, &path, None)
            .expect("create linked worktree");
        linked.push((
            dunce::canonicalize(path)
                .expect("canonicalize linked worktree")
                .to_string_lossy()
                .into_owned(),
            name,
        ));
    }
    let db_only_label = "session-list-db-only".to_owned();
    let db_only_cwd = canonical_worktree_base
        .join(&db_only_label)
        .to_string_lossy()
        .into_owned();
    assert!(!Path::new(&db_only_cwd).exists());

    let db = WorktreeDb::open(home).expect("open isolated worktree database");
    for (index, (path, label)) in linked.iter().enumerate().take(DB_OVERLAP_WORKTREE_COUNT) {
        db.register(&WorktreeRecord {
            id: label.clone(),
            path: PathBuf::from(path),
            source_repo: main.clone(),
            repo_name: "session-list-benchmark".to_owned(),
            kind: WorktreeKind::Session,
            creation_mode: "linked".to_owned(),
            git_ref: None,
            head_commit: Some(commit_id.to_string()),
            session_id: None,
            creator_pid: None,
            created_at: index as i64 + 1,
            last_accessed_at: None,
            status: WorktreeStatus::Alive,
            metadata: Some(echo_agent_runtime::session::worktree::build_label_metadata(
                label, false,
            )),
        })
        .expect("register linked worktree");
    }
    db.register(&WorktreeRecord {
        id: db_only_label.clone(),
        path: PathBuf::from(&db_only_cwd),
        source_repo: main.clone(),
        repo_name: "session-list-benchmark".to_owned(),
        kind: WorktreeKind::Session,
        creation_mode: "linked".to_owned(),
        git_ref: None,
        head_commit: Some(commit_id.to_string()),
        session_id: None,
        creator_pid: None,
        created_at: DB_TRACKED_WORKTREE_COUNT as i64,
        last_accessed_at: None,
        status: WorktreeStatus::Dead,
        metadata: Some(echo_agent_runtime::session::worktree::build_label_metadata(
            &db_only_label,
            false,
        )),
    })
    .expect("register DB-only worktree");
    let tracked = db
        .list(&ListFilter {
            source_repo: Some(main.clone()),
            include_dead: true,
            ..ListFilter::default()
        })
        .expect("list registered worktrees");
    assert_eq!(tracked.len(), DB_TRACKED_WORKTREE_COUNT);
    let overlap_cwd = linked[0].0.clone();
    let filesystem_only_cwd = linked[DB_OVERLAP_WORKTREE_COUNT].0.clone();
    assert!(
        tracked
            .iter()
            .any(|record| record.path == Path::new(&overlap_cwd))
    );
    assert!(
        tracked
            .iter()
            .all(|record| record.path != Path::new(&filesystem_only_cwd))
    );
    assert!(
        tracked
            .iter()
            .any(|record| record.path == Path::new(&db_only_cwd))
    );
    for record in &tracked {
        let expected_label = record
            .path
            .file_name()
            .expect("worktree basename")
            .to_string_lossy();
        assert_eq!(
            record
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("label"))
                .and_then(|label| label.as_str()),
            Some(expected_label.as_ref())
        );
    }

    let mut cwds = vec![main.to_string_lossy().into_owned()];
    let mut labels = vec![None];
    let mut sources = vec![CandidateSource::Main];
    for (path, label) in linked.iter().take(DB_OVERLAP_WORKTREE_COUNT) {
        cwds.push(path.clone());
        labels.push(Some(label.clone()));
        sources.push(CandidateSource::Overlap);
    }
    cwds.push(db_only_cwd.clone());
    labels.push(Some(db_only_label));
    sources.push(CandidateSource::DbOnly);
    for (path, label) in linked.iter().skip(DB_OVERLAP_WORKTREE_COUNT) {
        cwds.push(path.clone());
        labels.push(Some(label.clone()));
        sources.push(CandidateSource::FilesystemOnly);
    }
    assert_eq!(cwds.len(), SAME_REPO_CANDIDATE_COUNT);
    assert_eq!(labels.len(), SAME_REPO_CANDIDATE_COUNT);
    assert_eq!(sources.len(), SAME_REPO_CANDIDATE_COUNT);

    SameRepoTopology {
        cwds,
        labels,
        sources,
    }
}

fn same_repo_activity_offset(workspace_index: usize, session_index: usize) -> usize {
    session_index * SAME_REPO_CANDIDATE_COUNT
        + (workspace_index + ACTIVITY_ROTATION) % SAME_REPO_CANDIDATE_COUNT
}

fn sort_expected_sessions(sessions: &mut [ExpectedSession]) {
    sessions.sort_by(|a, b| b.active_at.cmp(&a.active_at).then_with(|| a.id.cmp(&b.id)));
}

fn benchmark_unrelated_cwd(index: usize) -> String {
    format!("/benchmark/unrelated-workspaces/workspace-{index:04}")
}

fn fixture_id() -> String {
    format!(
        "schema_v{FIXTURE_SCHEMA_VERSION}_sources_main{MAIN_CHECKOUT_COUNT}_\
         overlap{DB_OVERLAP_WORKTREE_COUNT}_db_only{DB_ONLY_WORKTREE_COUNT}_\
         fs_only{FILESYSTEM_ONLY_WORKTREE_COUNT}_labels_current_activity_interleaved_r\
         {ACTIVITY_ROTATION}_{WORKSPACE_COUNT}_workspaces_{SAME_REPO_CANDIDATE_COUNT}_\
         same_repo_cwds_{TOTAL_SUMMARY_COUNT}_summaries"
    )
}

fn write_summary(
    cwd_dir: &Path,
    cwd: &str,
    session_id: &str,
    active_at: DateTime<Utc>,
    ordinal: usize,
    worktree_label: Option<&str>,
) {
    let session_dir = cwd_dir.join(session_id);
    fs::create_dir(&session_dir).expect("create session directory");
    let summary = Summary {
        info: Info {
            id: acp::SessionId::new(session_id),
            cwd: cwd.to_owned(),
        },
        cwd_generation: 0,
        previous_cwd: None,
        pending_cwd_switch_reminder: None,
        cwd_switch_bookkeeping_generation: 0,
        session_summary: format!("Deterministic benchmark session {ordinal}"),
        created_at: active_at - ChronoDuration::minutes(5),
        updated_at: active_at,
        num_messages: 8 + ordinal % 24,
        num_chat_messages: 8 + ordinal % 24,
        current_model_id: acp::ModelId::new("benchmark-model"),
        parent_session_id: None,
        forked_at: None,
        collection_id: None,
        next_trace_turn: 0,
        chat_format_version: 1,
        prompt_display_cwd: None,
        session_kind: None,
        fork_context_source: None,
        fork_parent_prompt_id: None,
        inherited_prefix_len: None,
        hidden: None,
        source_workspace_dir: None,
        git_root_dir: Some(cwd.to_owned()),
        git_remotes: vec!["git@github.com:example/benchmark.git".to_owned()],
        head_commit: Some(format!("{ordinal:040x}")),
        head_branch: Some("main".to_owned()),
        request_id: None,
        echo_agent_home: None,
        last_active_at: Some(active_at),
        generated_title: Some(format!("Benchmark session {ordinal}")),
        title_is_manual: false,
        worktree_label: worktree_label.map(str::to_owned),
        agent_name: Some("benchmark-agent".to_owned()),
        sandbox_profile: Some("workspace".to_owned()),
        reasoning_effort: None,
        last_turn_summary: None,
        last_turn_summary_prompt_id: None,
        last_recap: None,
    };
    let summary_path = session_dir.join("summary.json");
    let bytes = serde_json::to_vec_pretty(&summary).expect("serialize summary");
    fs::write(&summary_path, bytes).expect("write summary");
    let mtime = FileTime::from_unix_time(active_at.timestamp(), 0);
    set_file_mtime(summary_path, mtime).expect("set summary mtime");
}

fn summary_ids(summaries: &[Summary]) -> Vec<String> {
    summaries
        .iter()
        .map(|summary| summary.info.id.to_string())
        .collect()
}

async fn build_local_list_with_delayed_peer(cwd: String) -> UnifiedListResult {
    let local = build_unified_list(
        None,
        None,
        ListReq {
            cwd: Some(cwd),
            limit: Some(RECENT_LIMIT),
            ..ListReq::default()
        },
    );
    let delayed_peer = async {
        tokio::time::sleep(COOPERATIVE_PEER_DELAY).await;
    };
    let (result, ()) = tokio::join!(biased; local, delayed_peer);
    result
}

fn bench_session_list(c: &mut Criterion) {
    let home = TempDir::new().expect("create fixture root");
    // SAFETY: no runtime or benchmark worker activity exists during setup.
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", home.path());
    }
    assert_eq!(
        echo_agent_runtime::util::echo_agent_home::echo_agent_home(),
        home.path()
    );
    let fixture = Fixture::new(home);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create runtime");
    fixture.assert_correct(&runtime);

    eprintln!(
        "session-list fixture: {WORKSPACE_COUNT} workspaces, {TOTAL_SUMMARY_COUNT} summaries, \
         {SAME_REPO_CANDIDATE_COUNT} same-repo cwd candidates, \
         {SAME_REPO_SUMMARY_COUNT} same-repo summaries; repeated samples use a warm process and \
         warm OS page cache"
    );
    let fixture_id = fixture_id();

    let mut storage = c.benchmark_group("session_list_storage_diagnostics_warm_os_page_cache");
    storage
        .sample_size(SAMPLE_SIZE)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_secs(1));

    storage.measurement_time(Duration::from_secs(120));
    storage.throughput(Throughput::Elements(TOTAL_SUMMARY_COUNT as u64));
    storage.bench_function(
        BenchmarkId::new("full_all_workspaces_json", &fixture_id),
        |b| {
            b.iter_with_large_drop(|| {
                black_box(
                    runtime
                        .block_on(fixture.adapter.list_sessions(None))
                        .expect("list all sessions"),
                )
            })
        },
    );

    storage.measurement_time(Duration::from_secs(5));
    storage.throughput(Throughput::Elements(SESSIONS_PER_SAME_REPO_CWD as u64));
    storage.bench_function(BenchmarkId::new("cwd_scoped_json", &fixture_id), |b| {
        b.iter_with_large_drop(|| {
            black_box(
                runtime
                    .block_on(
                        fixture
                            .adapter
                            .list_sessions(Some(black_box(&fixture.picker_cwd))),
                    )
                    .expect("list cwd sessions"),
            )
        })
    });

    // The `/session-info` title path: one summary loaded by (cwd, id).
    storage.measurement_time(Duration::from_secs(5));
    storage.throughput(Throughput::Elements(1));
    storage.bench_function(BenchmarkId::new("single_summary_load", &fixture_id), |b| {
        let info = Info {
            id: acp::SessionId::new("bench-session-0000-00"),
            cwd: fixture.picker_cwd.clone(),
        };
        b.iter_with_large_drop(|| {
            black_box(
                runtime
                    .block_on(fixture.adapter.load_summary(black_box(&info)))
                    .expect("load single summary"),
            )
        })
    });

    storage.measurement_time(Duration::from_secs(20));
    storage.throughput(Throughput::Elements(TOTAL_SUMMARY_COUNT as u64));
    storage.bench_function(
        BenchmarkId::new(
            format!("recent_stat_first_limit_{RECENT_LIMIT}"),
            &fixture_id,
        ),
        |b| {
            b.iter_with_large_drop(|| {
                black_box(
                    runtime
                        .block_on(fixture.adapter.list_sessions_recent(RECENT_LIMIT))
                        .expect("list recent sessions"),
                )
            })
        },
    );
    storage.finish();

    let mut shell = c.benchmark_group("session_list_shell_core_repeated_warm_process");
    shell
        .sample_size(SAMPLE_SIZE)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(30))
        .throughput(Throughput::Elements(SAME_REPO_SUMMARY_COUNT as u64));
    shell.bench_function(
        BenchmarkId::new(format!("local_only_cwd_limit_{RECENT_LIMIT}"), &fixture_id),
        |b| {
            b.iter_with_large_drop(|| {
                black_box(runtime.block_on(build_unified_list(
                    None,
                    None,
                    ListReq {
                        cwd: Some(black_box(fixture.picker_cwd.clone())),
                        limit: Some(RECENT_LIMIT),
                        ..ListReq::default()
                    },
                )))
            })
        },
    );
    shell.bench_function(
        BenchmarkId::new(
            format!(
                "cooperative_overlap_local_only_cwd_limit_{RECENT_LIMIT}_peer_delay_{}ms",
                COOPERATIVE_PEER_DELAY.as_millis()
            ),
            &fixture_id,
        ),
        |b| {
            b.iter_with_large_drop(|| {
                black_box(
                    runtime.block_on(build_local_list_with_delayed_peer(black_box(
                        fixture.picker_cwd.clone(),
                    ))),
                )
            })
        },
    );
    shell.finish();
}

criterion_group!(benches, bench_session_list);
criterion_main!(benches);
