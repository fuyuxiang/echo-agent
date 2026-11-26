use std::os::fd::AsRawFd as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};
use std::time::{Duration, Instant};

use echo_agent_test_support::{TestProcess, TestProcessConfig, TestSandbox};

const CHILD_ENV: &str = "ECHO_AGENT_MEMTRACE_SIGNAL_TOPOLOGY_CHILD";
const CHILD_SUCCESS_MARKER: &str = "ECHO_AGENT_MEMTRACE_SIGNAL_TOPOLOGY_OK";
const ISOLATED_TEST_NAME: &str = concat!(
    "memory_trace::memory_trace_signal_topology_tests::",
    "memtrace_wait_survives_outer_sigchld_errno_clobber"
);
const SAMPLE_INTERVAL_SECS: &str = "1";
const SIGNAL_START_DELAY: Duration = Duration::from_millis(25);
const SIGNAL_RETRY_INTERVAL: Duration = Duration::from_millis(1);
const HANDLER_DEADLINE: Duration = Duration::from_secs(5);
const CHILD_DEADLINE: Duration = Duration::from_secs(15);
const SAMPLE_POLL_INTERVAL: Duration = Duration::from_millis(20);

static OUTER_HANDLER_RAN: AtomicBool = AtomicBool::new(false);
static EAGAIN_SURVIVED_CHAIN: AtomicBool = AtomicBool::new(false);
static REGISTRY_CALLBACK_RAN: AtomicBool = AtomicBool::new(false);
static PIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static PREVIOUS_HANDLER: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

#[tokio::test(flavor = "current_thread")]
async fn memtrace_wait_survives_outer_sigchld_errno_clobber() {
    if std::env::var_os(CHILD_ENV).is_some() {
        run_isolated_topology();
        println!("{CHILD_SUCCESS_MARKER}");
        return;
    }

    let mut command =
        tokio::process::Command::new(std::env::current_exe().expect("resolve current test binary"));
    command.args(["--exact", ISOLATED_TEST_NAME, "--nocapture"]);

    let sandbox = TestSandbox::new();
    let mut config = TestProcessConfig::new()
        .label("isolated memtrace signal topology")
        .env(CHILD_ENV, "1")
        .env("ECHO_AGENT_MEMTRACE", "1")
        .env("ECHO_AGENT_MEMTRACE_INTERVAL_SECS", SAMPLE_INTERVAL_SECS);
    if let Some(scale) = std::env::var_os("ECHO_AGENT_TEST_TIMEOUT_SCALE") {
        config = config.env("ECHO_AGENT_TEST_TIMEOUT_SCALE", scale);
    }

    let mut child =
        TestProcess::spawn(command, &sandbox, config).expect("spawn isolated signal topology test");
    let child_deadline = echo_agent_test_support::scaled(CHILD_DEADLINE);
    let status = match child.wait_with_deadline(child_deadline).await {
        Ok(Some(status)) => status,
        Ok(None) => {
            let before_kill = child.diagnostic_summary();
            child.kill().await.unwrap_or_else(|error| {
                panic!(
                    "failed to hard-kill/reap child after {child_deadline:?}: {error}\n\
                     before kill: {before_kill}\nafter kill: {}",
                    child.diagnostic_summary(),
                )
            });
            panic!(
                "isolated child exceeded {child_deadline:?}; hard-killed and reaped\n\
                 before kill: {before_kill}\nafter kill: {}",
                child.diagnostic_summary(),
            );
        }
        Err(error) => {
            let before_kill = child.diagnostic_summary();
            child.kill().await.unwrap_or_else(|kill_error| {
                panic!(
                    "isolated child wait failed: {error}; hard-kill/reap failed: {kill_error}\n\
                     before kill: {before_kill}\nafter kill: {}",
                    child.diagnostic_summary(),
                )
            });
            panic!(
                "isolated child wait failed: {error}; hard-killed and reaped\n\
                 before kill: {before_kill}\nafter kill: {}",
                child.diagnostic_summary(),
            );
        }
    };

    assert!(
        status.success(),
        "isolated child exited abnormally: {status:?}\n{}",
        child.diagnostic_summary(),
    );
    assert!(
        child
            .stdout_tail()
            .text
            .lines()
            .any(|line| line == CHILD_SUCCESS_MARKER),
        "isolated child omitted success marker\n{}",
        child.diagnostic_summary(),
    );
}

fn run_isolated_topology() {
    // SAFETY: The callback performs only a lock-free atomic store.
    let registry_id = unsafe {
        signal_hook::low_level::register(libc::SIGCHLD, || {
            REGISTRY_CALLBACK_RAN.store(true, Ordering::SeqCst);
        })
    }
    .expect("register inner SIGCHLD callback");

    let (read, write) = std::os::unix::net::UnixStream::pair().expect("create self-pipe pair");
    read.set_nonblocking(true).expect("make reader nonblocking");
    write
        .set_nonblocking(true)
        .expect("make writer nonblocking");
    fill_nonblocking_descriptor(write.as_raw_fd());
    PIPE_WRITE_FD.store(write.as_raw_fd(), Ordering::SeqCst);
    install_outer_handler();

    let dir = tempfile::tempdir().expect("memtrace output dir");
    // Drive the production sampler path so a revert of the wait call site fails.
    super::start(dir.path().to_path_buf());

    // errno is thread-local and the vulnerable sleep assert runs on the
    // sampler; target that thread with pthread_kill rather than process-wide
    // kill (which can land on the main/tokio thread and miss the waiter).
    let sampler_pthread = wait_for_sampler_pthread();
    let notifier = std::thread::spawn(move || {
        std::thread::park_timeout(SIGNAL_START_DELAY);
        let observation_started = Instant::now();
        let handler_deadline = echo_agent_test_support::scaled(HANDLER_DEADLINE);
        while !required_properties_observed() {
            // SAFETY: sampler_pthread is the live echo-agent-memtrace thread published
            // after start(); SIGCHLD is handled in this isolated child only.
            assert_eq!(
                unsafe { libc::pthread_kill(sampler_pthread as libc::pthread_t, libc::SIGCHLD) },
                0,
            );
            if required_properties_observed() {
                break;
            }
            let remaining = handler_deadline.saturating_sub(observation_started.elapsed());
            if remaining.is_zero() {
                break;
            }
            std::thread::park_timeout(SIGNAL_RETRY_INTERVAL.min(remaining));
        }
    });

    let sample_deadline = echo_agent_test_support::scaled(CHILD_DEADLINE);
    let sample_started = Instant::now();
    let mut saw_sampler_event = false;
    while sample_started.elapsed() < sample_deadline {
        if memtrace_has_sampler_event(dir.path()) {
            saw_sampler_event = true;
            break;
        }
        // Keep interrupting the sampler wait so a sleep-based loop would still
        // trip the errno-clobber abort path if the production wait were reverted.
        // SAFETY: same live sampler pthread as the notifier thread.
        let _ = unsafe { libc::pthread_kill(sampler_pthread as libc::pthread_t, libc::SIGCHLD) };
        std::thread::park_timeout(SAMPLE_POLL_INTERVAL);
    }
    notifier.join().expect("join signal notifier");

    assert!(signal_hook::low_level::unregister(registry_id));
    drop((read, write));
    assert!(
        OUTER_HANDLER_RAN.load(Ordering::SeqCst),
        "outer SIGCHLD handler did not run before its deadline",
    );
    assert!(
        EAGAIN_SURVIVED_CHAIN.load(Ordering::SeqCst),
        "EAGAIN did not survive the chained registry handler",
    );
    assert!(
        REGISTRY_CALLBACK_RAN.load(Ordering::SeqCst),
        "signal-hook registry callback did not run",
    );
    assert!(
        saw_sampler_event,
        "production sampler never wrote a start/sample event under SIGCHLD errno clobber",
    );
}

fn wait_for_sampler_pthread() -> usize {
    let deadline = echo_agent_test_support::scaled(HANDLER_DEADLINE);
    let started = Instant::now();
    loop {
        if let Some(pthread) = super::test_only_sampler_pthread() {
            return pthread;
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        assert!(
            !remaining.is_zero(),
            "sampler thread never published pthread_t after start()",
        );
        std::thread::park_timeout(SIGNAL_RETRY_INTERVAL.min(remaining));
    }
}

fn memtrace_has_sampler_event(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in body.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if matches!(value["kind"].as_str(), Some("start") | Some("sample")) {
                return true;
            }
        }
    }
    false
}

fn required_properties_observed() -> bool {
    OUTER_HANDLER_RAN.load(Ordering::SeqCst)
        && EAGAIN_SURVIVED_CHAIN.load(Ordering::SeqCst)
        && REGISTRY_CALLBACK_RAN.load(Ordering::SeqCst)
}

fn fill_nonblocking_descriptor(fd: libc::c_int) {
    let bytes = [0_u8; 4096];
    loop {
        // SAFETY: fd is the live writer and bytes remains valid for this nonblocking write.
        let result = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if result >= 0 {
            continue;
        }
        assert_eq!(get_errno(), libc::EAGAIN, "self-pipe must be full");
        return;
    }
}

fn install_outer_handler() {
    // SAFETY: sigaction values are initialized, and no signal is sent until the prior handler is
    // published after this single-threaded setup step. The handler itself null-checks the
    // previous pointer so a race cannot call through a null function pointer.
    unsafe {
        let mut outer: libc::sigaction = std::mem::zeroed();
        let mut previous: libc::sigaction = std::mem::zeroed();
        outer.sa_sigaction = outer_sigchld_handler as *const () as usize;
        outer.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART | libc::SA_NOCLDSTOP;
        libc::sigemptyset(&mut outer.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGCHLD, &outer, &mut previous), 0);
        assert_ne!(previous.sa_flags & libc::SA_SIGINFO, 0);
        assert_ne!(previous.sa_sigaction, libc::SIG_DFL);
        assert_ne!(previous.sa_sigaction, libc::SIG_IGN);
        PREVIOUS_HANDLER.store(previous.sa_sigaction as *mut (), Ordering::SeqCst);
    }
}

extern "C" fn outer_sigchld_handler(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    let byte = [0_u8; 1];
    // SAFETY: The descriptor and byte stay live until every signal handler has completed; write
    // is async-signal-safe and nonblocking, so the intentionally full descriptor cannot deadlock.
    let result = unsafe {
        libc::write(
            PIPE_WRITE_FD.load(Ordering::SeqCst),
            byte.as_ptr().cast(),
            byte.len(),
        )
    };
    let write_saw_eagain = result == -1 && get_errno() == libc::EAGAIN;

    let handler = PREVIOUS_HANDLER.load(Ordering::SeqCst);
    if !handler.is_null() {
        // SAFETY: install_outer_handler verified the SA_SIGINFO ABI and excluded default and
        // ignored dispositions before preserving the exact prior signal-hook-registry function
        // pointer. The null check above covers the publish window after sigaction.
        let previous: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
            unsafe { std::mem::transmute::<*mut (), _>(handler) };
        previous(signal, info, context);
    }
    if write_saw_eagain && get_errno() == libc::EAGAIN {
        EAGAIN_SURVIVED_CHAIN.store(true, Ordering::SeqCst);
    }
    OUTER_HANDLER_RAN.store(true, Ordering::SeqCst);
}

#[cfg(target_os = "linux")]
fn get_errno() -> libc::c_int {
    // SAFETY: __errno_location returns the interrupted thread's valid errno pointer.
    unsafe { *libc::__errno_location() }
}

#[cfg(target_os = "macos")]
fn get_errno() -> libc::c_int {
    // SAFETY: __error returns the interrupted thread's valid errno pointer.
    unsafe { *libc::__error() }
}
