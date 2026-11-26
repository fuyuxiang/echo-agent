use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tempfile::TempDir;

use super::{
    EnsureCommitsOutcome, FetchChild, MAX_FETCH_STDERR_BYTES, RESTORE_FETCH_BASE_RESERVE,
    RESTORE_FETCH_BUDGET, RESTORE_FETCH_TEARDOWN_RESERVE, RestoreGit, ensure_commits_reachable,
    ensure_commits_reachable_with, fetch_if_missing, is_full_object_id, is_safe_git_ref,
    origin_fetch_spec_for_checkout_target, targeted_fetch_args, targeted_fetch_command,
};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER: &str = "cccccccccccccccccccccccccccccccccccccccc";

struct FakeGit {
    present: Mutex<HashSet<String>>,
    fetches: Mutex<Vec<(String, Duration)>>,
    fetch_err: Option<String>,
    fetch_err_by_oid: std::collections::HashMap<String, String>,
    also_materialize: Vec<String>,
}

impl FakeGit {
    fn with_present(oids: &[&str]) -> Self {
        Self {
            present: Mutex::new(oids.iter().map(|s| (*s).to_owned()).collect()),
            fetches: Mutex::new(Vec::new()),
            fetch_err: None,
            fetch_err_by_oid: std::collections::HashMap::new(),
            also_materialize: Vec::new(),
        }
    }

    fn fetched_oids(&self) -> Vec<String> {
        self.fetches
            .lock()
            .expect("fetches lock")
            .iter()
            .map(|(oid, _)| oid.clone())
            .collect()
    }
}

impl RestoreGit for FakeGit {
    fn has_object(&self, _repo: &Path, oid: &str) -> bool {
        self.present.lock().expect("present lock").contains(oid)
    }

    fn fetch_oid(&self, _repo: &Path, oid: &str, timeout: Duration) -> Result<()> {
        self.fetches
            .lock()
            .expect("fetches lock")
            .push((oid.to_owned(), timeout));
        if let Some(err) = self.fetch_err_by_oid.get(oid) {
            bail!("{err}");
        }
        if let Some(err) = &self.fetch_err {
            bail!("{err}");
        }
        let mut present = self.present.lock().expect("present lock");
        present.insert(oid.to_owned());
        for extra in &self.also_materialize {
            present.insert(extra.clone());
        }
        Ok(())
    }
}

fn deadline() -> Instant {
    Instant::now() + RESTORE_FETCH_BUDGET
}

fn assert_timeout_about(actual: Duration, target: Duration) {
    assert!(actual <= target, "timeout {actual:?} exceeds {target:?}");
    assert!(
        actual >= target.saturating_sub(Duration::from_secs(2)),
        "timeout {actual:?} is not close to {target:?}"
    );
}

#[test]
fn is_full_object_id_boundary_table() {
    let cases = [
        ("", false),
        ("deadbeef", false),
        ("origin/main", false),
        ("HEAD", false),
        (HEAD, true),
        ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaag", false),
        ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaA", false),
        ("0123456789abcdef0123456789ABCDEF01234567", false),
        (
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            true,
        ),
        (
            "0123456789abcdef0123456789ABCDEF0123456789abcdef0123456789ABCDEF",
            false,
        ),
    ];
    for (value, expected) in cases {
        assert_eq!(
            is_full_object_id(value),
            expected,
            "oid {value:?} expected {expected}"
        );
    }
    assert!(!is_full_object_id(&"a".repeat(39)));
    assert!(!is_full_object_id(&"a".repeat(41)));
    assert!(!is_full_object_id(&"a".repeat(63)));
    assert!(is_full_object_id(&"a".repeat(64)));
}

#[test]
fn head_and_base_present_skips_fetch() {
    let git = FakeGit::with_present(&[HEAD, BASE]);
    let outcome = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::AlreadyPresent);
    assert!(git.fetched_oids().is_empty());
}

#[test]
fn head_missing_base_present_fetches_only_head() {
    let git = FakeGit::with_present(&[BASE]);
    let outcome = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    assert_eq!(git.fetched_oids(), vec![HEAD.to_owned()]);
}

#[test]
fn head_present_base_missing_fetches_only_base() {
    let git = FakeGit::with_present(&[HEAD]);
    let outcome = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    assert_eq!(git.fetched_oids(), vec![BASE.to_owned()]);
}

#[test]
fn both_missing_fetches_head_then_base() {
    let git = FakeGit::with_present(&[]);
    let outcome = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    assert_eq!(git.fetched_oids(), vec![HEAD.to_owned(), BASE.to_owned()]);
}

#[test]
fn head_fetch_that_materializes_base_skips_base_fetch() {
    let mut git = FakeGit::with_present(&[]);
    git.also_materialize = vec![BASE.to_owned()];
    let outcome = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    assert_eq!(git.fetched_oids(), vec![HEAD.to_owned()]);
}

#[test]
fn same_head_and_base_fetches_once() {
    let git = FakeGit::with_present(&[]);
    let outcome = ensure_commits_reachable_with(Path::new("/repo"), HEAD, HEAD, &git, deadline())
        .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    let fetches = git.fetches.lock().expect("fetches lock");
    assert_eq!(fetches.len(), 1);
    assert_eq!(fetches[0].0, HEAD);
    // Identical oids do not reserve a second fetch slice.
    assert_timeout_about(fetches[0].1, RESTORE_FETCH_BUDGET);
}

#[test]
fn invalid_oids_are_not_fetched() {
    let git = FakeGit::with_present(&[]);
    let outcome =
        ensure_commits_reachable_with(Path::new("/repo"), "origin/main", "HEAD", &git, deadline())
            .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::SkippedInvalidOid);
    assert!(git.fetched_oids().is_empty());
}

#[test]
fn mixed_valid_present_and_invalid_oid_is_already_present() {
    let git = FakeGit::with_present(&[HEAD]);
    let outcome =
        ensure_commits_reachable_with(Path::new("/repo"), HEAD, "origin/main", &git, deadline())
            .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::AlreadyPresent);
    assert!(git.fetched_oids().is_empty());
}

#[test]
fn mixed_valid_missing_and_invalid_oid_fetches_only_valid() {
    let git = FakeGit::with_present(&[]);
    let outcome =
        ensure_commits_reachable_with(Path::new("/repo"), HEAD, "not-a-ref", &git, deadline())
            .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    assert_eq!(git.fetched_oids(), vec![HEAD.to_owned()]);
}

#[test]
fn mixed_invalid_head_and_valid_missing_base_fetches_only_base() {
    let git = FakeGit::with_present(&[]);
    let outcome =
        ensure_commits_reachable_with(Path::new("/repo"), "origin/main", BASE, &git, deadline())
            .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    assert_eq!(git.fetched_oids(), vec![BASE.to_owned()]);
}

#[test]
fn is_safe_git_ref_table() {
    let cases = [
        ("main", true),
        ("origin/main", true),
        ("feature/foo-bar", true),
        ("refs/heads/main", true),
        ("", false),
        ("-main", false),
        ("foo:bar", false),
        ("foo..bar", false),
        ("foo@{u}", false),
        ("foo bar", false),
        ("foo*bar", false),
        ("HEAD", true),
    ];
    for (value, expected) in cases {
        assert_eq!(is_safe_git_ref(value), expected, "ref {value:?}");
    }
}

#[test]
fn origin_fetch_spec_maps_local_tracking_names() {
    let cases = [
        (HEAD, Some(HEAD)),
        ("main", Some("main")),
        ("feature/foo-bar", Some("feature/foo-bar")),
        ("origin/main", Some("main")),
        ("origin/feature/foo", Some("feature/foo")),
        ("refs/remotes/origin/main", Some("main")),
        ("refs/remotes/origin/feature/foo", Some("feature/foo")),
        ("refs/heads/main", Some("refs/heads/main")),
        ("refs/tags/v1.0.0", Some("refs/tags/v1.0.0")),
        ("HEAD", Some("HEAD")),
        ("origin/", None),
        ("refs/remotes/origin/", None),
        ("foo:bar", None),
        ("", None),
        ("-main", None),
        ("abc1234", None),
        ("ABC1234", None),
        ("aaaaaaaa", None),
        (&HEAD[..7], None),
        (&HEAD[..39], None),
    ];
    for (value, expected) in cases {
        assert_eq!(
            origin_fetch_spec_for_checkout_target(value),
            expected,
            "target {value:?}"
        );
    }
}

#[test]
fn expired_shared_budget_skips_fetch() {
    let git = FakeGit::with_present(&[]);
    let err = fetch_if_missing(Path::new("/repo"), HEAD, &git, Duration::ZERO)
        .expect_err("budget exhausted");
    assert!(err.to_string().contains("budget exhausted"), "got: {err}");
    assert!(git.fetched_oids().is_empty());
    let err = ensure_commits_reachable_with(
        Path::new("/repo"),
        HEAD,
        BASE,
        &git,
        Instant::now() - Duration::from_secs(1),
    )
    .expect_err("deadline in the past");
    assert!(err.to_string().contains("budget exhausted"), "got: {err}");
    assert!(git.fetched_oids().is_empty());
}

#[test]
fn fetch_failure_is_returned_after_attempting_base() {
    let mut git = FakeGit::with_present(&[]);
    git.fetch_err = Some("network down".to_owned());
    let err = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect_err("fetch should fail");
    assert!(err.to_string().contains("network down"), "got: {err}");
    assert!(
        err.to_string().contains("public-base") || err.chain().count() > 1,
        "both oid failures should surface, got: {err:#}"
    );
    assert_eq!(git.fetched_oids(), vec![HEAD.to_owned(), BASE.to_owned()]);
}

#[test]
fn both_fetch_errors_are_combined() {
    let mut git = FakeGit::with_present(&[]);
    git.fetch_err_by_oid
        .insert(HEAD.to_owned(), "head boom".to_owned());
    git.fetch_err_by_oid
        .insert(BASE.to_owned(), "base boom".to_owned());
    let err = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect_err("fetch should fail");
    let text = format!("{err:#}");
    assert!(text.contains("head boom"), "got: {text}");
    assert!(text.contains("base boom"), "got: {text}");
}

#[test]
fn fetch_if_missing_records_timeout_budget() {
    let git = FakeGit::with_present(&[]);
    let timeout = Duration::from_secs(12);
    fetch_if_missing(Path::new("/repo"), OTHER, &git, timeout).expect("fetch");
    let fetches = git.fetches.lock().expect("fetches lock");
    assert_eq!(fetches.as_slice(), &[(OTHER.to_owned(), timeout)]);
}

#[test]
fn both_missing_head_timeout_reserves_base_slice() {
    let git = FakeGit::with_present(&[]);
    let outcome = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    let fetches = git.fetches.lock().expect("fetches lock");
    assert_eq!(fetches.len(), 2);
    assert_eq!(fetches[0].0, HEAD);
    assert_eq!(fetches[1].0, BASE);
    assert_timeout_about(
        fetches[0].1,
        RESTORE_FETCH_BUDGET
            .saturating_sub(RESTORE_FETCH_BASE_RESERVE)
            .saturating_sub(RESTORE_FETCH_TEARDOWN_RESERVE),
    );
    assert!(
        fetches[0].1 + RESTORE_FETCH_TEARDOWN_RESERVE + RESTORE_FETCH_BASE_RESERVE
            <= RESTORE_FETCH_BUDGET,
        "head timeout {:?} + teardown {:?} must leave base reserve {:?}",
        fetches[0].1,
        RESTORE_FETCH_TEARDOWN_RESERVE,
        RESTORE_FETCH_BASE_RESERVE,
    );
    assert!(
        fetches[1].1 >= RESTORE_FETCH_BASE_RESERVE,
        "base timeout {:?} should keep at least the reserve",
        fetches[1].1
    );
}

#[test]
fn head_fetch_uses_full_budget_when_base_already_present() {
    let git = FakeGit::with_present(&[BASE]);
    let outcome = ensure_commits_reachable_with(Path::new("/repo"), HEAD, BASE, &git, deadline())
        .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    let fetches = git.fetches.lock().expect("fetches lock");
    assert_eq!(fetches.len(), 1);
    assert_eq!(fetches[0].0, HEAD);
    assert_timeout_about(fetches[0].1, RESTORE_FETCH_BUDGET);
}

#[test]
fn head_fetch_uses_full_budget_when_base_is_not_an_oid() {
    let git = FakeGit::with_present(&[]);
    let outcome =
        ensure_commits_reachable_with(Path::new("/repo"), HEAD, "origin/main", &git, deadline())
            .expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    let fetches = git.fetches.lock().expect("fetches lock");
    assert_eq!(fetches.len(), 1);
    assert_eq!(fetches[0].0, HEAD);
    assert_timeout_about(fetches[0].1, RESTORE_FETCH_BUDGET);
}

#[test]
fn low_remaining_budget_skips_head_to_reserve_base_fetch() {
    let git = FakeGit::with_present(&[]);
    let err = ensure_commits_reachable_with(
        Path::new("/repo"),
        HEAD,
        BASE,
        &git,
        Instant::now() + Duration::from_secs(5),
    )
    .expect_err("head skip fails ensure even if base is fetched");
    assert!(err.to_string().contains("budget exhausted"), "got: {err}");
    let fetches = git.fetches.lock().expect("fetches lock");
    assert_eq!(fetches.len(), 1, "only base should be fetched: {fetches:?}");
    assert_eq!(fetches[0].0, BASE);
    assert!(
        fetches[0].1 > Duration::ZERO && fetches[0].1 <= Duration::from_secs(5),
        "base timeout {:?}",
        fetches[0].1
    );
}

#[test]
fn targeted_fetch_args_exact_for_shallow_and_full() {
    assert_eq!(
        targeted_fetch_args(HEAD, true),
        vec![
            "fetch".to_owned(),
            "--no-tags".to_owned(),
            "--depth=1".to_owned(),
            "origin".to_owned(),
            HEAD.to_owned(),
        ]
    );
    assert_eq!(
        targeted_fetch_args(HEAD, false),
        vec![
            "fetch".to_owned(),
            "--no-tags".to_owned(),
            "origin".to_owned(),
            HEAD.to_owned(),
        ]
    );
    assert_eq!(
        targeted_fetch_args("refs/tags/v1.0.0", false),
        vec![
            "fetch".to_owned(),
            "--no-tags".to_owned(),
            "origin".to_owned(),
            "refs/tags/v1.0.0:refs/tags/v1.0.0".to_owned(),
        ]
    );
    assert_eq!(
        targeted_fetch_args("main", false),
        vec![
            "fetch".to_owned(),
            "--no-tags".to_owned(),
            "origin".to_owned(),
            "main".to_owned(),
        ]
    );
}

#[test]
fn targeted_fetch_command_argv_matches_args_helper() {
    for is_shallow in [true, false] {
        let cmd = targeted_fetch_command(Path::new("/repo"), HEAD, is_shallow);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, targeted_fetch_args(HEAD, is_shallow));
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/repo")));
        assert!(
            !args
                .iter()
                .any(|a| a == "--unshallow" || a == "--no-optional-locks"),
            "forbidden flags in {args:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn fetch_timeout_kills_process_group_with_grandchild() {
    use std::process::Stdio;
    use std::time::Instant;

    use echo_agent_tty_utils::detach_std_command;

    let dir = TempDir::new().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    let mut cmd = std::process::Command::new("sh");
    cmd.args([
        "-c",
        &format!("sleep 30 & echo $! >{}; exec sleep 30", pidfile.display()),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    detach_std_command(&mut cmd);

    let mut child = FetchChild::spawn(cmd).expect("spawn");
    let leader = child.child.as_ref().expect("child").id();
    let start = Instant::now();
    let err = child
        .wait_success(Duration::from_millis(250), HEAD)
        .expect_err("timeout");
    assert!(err.to_string().contains("timed out"), "got: {err}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "timeout must return promptly, elapsed {:?}",
        start.elapsed()
    );

    let grandchild: u32 = std::fs::read_to_string(&pidfile)
        .expect("grandchild pidfile")
        .trim()
        .parse()
        .expect("grandchild pid");
    assert!(!pid_alive(leader), "leader {leader} must not remain live");
    assert!(
        !pid_alive(grandchild),
        "grandchild {grandchild} must be killed via killpg"
    );
}

#[cfg(target_os = "linux")]
fn pid_alive(pid: u32) -> bool {
    let path = format!("/proc/{pid}/stat");
    let Ok(stat) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some((_, after_comm)) = stat.rsplit_once(')') else {
        return false;
    };
    !after_comm.trim_start().starts_with('Z')
}

#[cfg(all(unix, not(target_os = "linux")))]
fn pid_alive(pid: u32) -> bool {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    kill(Pid::from_raw(pid as i32), None::<Signal>).is_ok()
}

#[cfg(unix)]
#[test]
fn fetch_nonzero_exit_includes_stderr() {
    use std::process::Stdio;

    use echo_agent_tty_utils::detach_std_command;

    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "echo 'fatal: unsavoury object' >&2; exit 128"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    detach_std_command(&mut cmd);
    let err = FetchChild::spawn(cmd)
        .expect("spawn")
        .wait_success(Duration::from_secs(2), HEAD)
        .expect_err("nonzero");
    let text = err.to_string();
    assert!(text.contains("fatal: unsavoury object"), "got: {text}");
    assert!(
        text.contains("128") || text.contains("failed"),
        "got: {text}"
    );
}

#[cfg(unix)]
#[test]
fn fetch_stderr_is_truncated() {
    use std::process::Stdio;

    use echo_agent_tty_utils::detach_std_command;

    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "dd if=/dev/zero bs=9000 count=1 >&2; exit 1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    detach_std_command(&mut cmd);
    let err = FetchChild::spawn(cmd)
        .expect("spawn")
        .wait_success(Duration::from_secs(5), HEAD)
        .expect_err("nonzero");
    let text = err.to_string();
    assert!(text.contains("…(truncated)"), "got: {text}");
    assert!(
        text.len() <= MAX_FETCH_STDERR_BYTES + 256,
        "stderr must be capped, len {}",
        text.len()
    );
}

#[cfg(unix)]
#[test]
fn fetch_child_drop_tears_down_process() {
    use std::process::Stdio;
    use std::time::Instant;

    use echo_agent_tty_utils::detach_std_command;

    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    detach_std_command(&mut cmd);
    let child = FetchChild::spawn(cmd).expect("spawn");
    let pid = child.child.as_ref().expect("child").id();
    let start = Instant::now();
    drop(child);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "Drop teardown must not hang"
    );
    assert!(!pid_alive(pid), "dropped fetch child must die");
}

#[cfg(unix)]
#[test]
fn fetch_timeout_sigkills_term_immune_grandchild() {
    use std::process::Stdio;
    use std::time::Instant;

    use echo_agent_tty_utils::detach_std_command;

    let dir = TempDir::new().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    let mut cmd = std::process::Command::new("sh");
    cmd.args([
        "-c",
        &format!(
            "trap '' TERM; sh -c \"trap '' TERM; while true; do sleep 30; done\" & echo $! >{}; while true; do sleep 30; done",
            pidfile.display()
        ),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    detach_std_command(&mut cmd);

    let mut child = FetchChild::spawn(cmd).expect("spawn");
    let leader = child.child.as_ref().expect("child").id();
    let start = Instant::now();
    let err = child
        .wait_success(Duration::from_millis(400), HEAD)
        .expect_err("timeout");
    assert!(err.to_string().contains("timed out"), "got: {err}");
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(
        !pid_alive(leader),
        "TERM-ignoring leader {leader} must die on SIGKILL"
    );
    let grandchild: u32 = std::fs::read_to_string(&pidfile)
        .expect("pidfile")
        .trim()
        .parse()
        .expect("pid");
    assert!(
        !pid_alive(grandchild),
        "grandchild {grandchild} must not remain live"
    );
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    echo_agent_test_utils::require_git!();
    echo_agent_test_utils::git::run_git(dir, args)
}

fn init_upstream_two_commits(dir: &Path) -> (String, String) {
    echo_agent_test_utils::git::init_git_repo(dir);
    std::fs::write(dir.join("README.md"), "one\n").unwrap();
    echo_agent_test_utils::git::git_commit_all(dir, "one");
    let first = run_git(dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("README.md"), "two\n").unwrap();
    echo_agent_test_utils::git::git_commit_all(dir, "two");
    let second = run_git(dir, &["rev-parse", "HEAD"]);
    (first, second)
}

fn is_shallow(dir: &Path) -> bool {
    run_git(dir, &["rev-parse", "--is-shallow-repository"]) == "true"
}

fn abs(dir: &Path) -> PathBuf {
    dunce::canonicalize(dir).expect("canonicalize")
}

fn file_url(dir: &Path) -> String {
    format!("file://{}", abs(dir).display())
}

#[test]
fn hermetic_fetch_keeps_shallow_clone_shallow_and_makes_base_reachable() {
    let upstream = TempDir::new().unwrap();
    let (base, head) = init_upstream_two_commits(upstream.path());
    let dest = TempDir::new().unwrap();
    let dest_repo = dest.path().join("repo");
    echo_agent_test_utils::require_git!();
    echo_agent_test_utils::git::run_git(
        dest.path(),
        &[
            "clone",
            "--depth=1",
            &file_url(upstream.path()),
            dest_repo.to_str().unwrap(),
        ],
    );
    assert!(is_shallow(&dest_repo));
    assert!(run_git(&dest_repo, &["cat-file", "-t", &head]) == "commit");
    assert!(run_git_allow_fail(&dest_repo, &["cat-file", "-t", &base]).is_err());

    let outcome = ensure_commits_reachable(&dest_repo, &head, &base).expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    assert_eq!(run_git(&dest_repo, &["cat-file", "-t", &base]), "commit");
    assert!(is_shallow(&dest_repo), "depth-1 clone must stay shallow");
}

#[test]
fn hermetic_fetch_keeps_full_clone_unshallow() {
    let upstream = TempDir::new().unwrap();
    echo_agent_test_utils::git::init_git_repo(upstream.path());
    std::fs::write(upstream.path().join("README.md"), "one\n").unwrap();
    echo_agent_test_utils::git::git_commit_all(upstream.path(), "one");
    let base = run_git(upstream.path(), &["rev-parse", "HEAD"]);

    let dest = TempDir::new().unwrap();
    let dest_repo = dest.path().join("repo");
    echo_agent_test_utils::require_git!();
    echo_agent_test_utils::git::run_git(
        dest.path(),
        &[
            "clone",
            &file_url(upstream.path()),
            dest_repo.to_str().unwrap(),
        ],
    );
    assert!(!is_shallow(&dest_repo));

    std::fs::write(upstream.path().join("README.md"), "two\n").unwrap();
    echo_agent_test_utils::git::git_commit_all(upstream.path(), "two");
    let head = run_git(upstream.path(), &["rev-parse", "HEAD"]);

    let outcome = ensure_commits_reachable(&dest_repo, &head, &base).expect("ensure");
    assert_eq!(outcome, EnsureCommitsOutcome::Fetched);
    assert_eq!(run_git(&dest_repo, &["cat-file", "-t", &head]), "commit");
    assert!(
        !is_shallow(&dest_repo),
        "full clone must not become shallow"
    );
}

fn run_git_allow_fail(dir: &Path, args: &[&str]) -> Result<String, String> {
    echo_agent_test_utils::require_git!();
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}
