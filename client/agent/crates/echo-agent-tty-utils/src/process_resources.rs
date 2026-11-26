//! This process's resource gauges: memory, threads, open files, CPU, start time.

/// Fields are `None` where the platform offers no cheap equivalent. Open
/// files are Linux-only, matching what the resource soaks bound; threads are
/// reported on both Linux and macOS (a leaked-thread regression must be
/// visible in memtrace samples on the platform where it takes machines down).
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessResources {
    pub rss_bytes: Option<u64>,
    /// Since the process started, not since the last sample.
    pub peak_rss_bytes: Option<u64>,
    pub footprint_bytes: Option<u64>,
    pub threads: Option<u64>,
    pub open_files: Option<u64>,
}

/// Everything, including the Linux fd-directory scan.
pub fn sample_process_resources() -> ProcessResources {
    imp::sample()
}

/// Memory and thread gauges, leaving `open_files` unset. Skips the Linux fd
/// directory scan, for callers that sample on a timer: threads ride along
/// free (parsed from the same `/proc/self/status` read on Linux, one cheap
/// `proc_pidinfo` on macOS). The tiers only diverge on Linux — on macOS this
/// and [`sample_process_resources`] take the same sample.
pub fn sample_process_memory() -> ProcessResources {
    imp::sample_memory()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessCpu {
    pub self_time: Option<std::time::Duration>,
    pub self_user_time: Option<std::time::Duration>,
    pub self_system_time: Option<std::time::Duration>,
    /// Reaped children only; running children are invisible until they exit.
    pub children_time: Option<std::time::Duration>,
}

pub fn sample_process_cpu() -> ProcessCpu {
    cpu::sample()
}

pub fn process_start_time() -> Option<std::time::SystemTime> {
    imp::start_time()
}

/// cgroup v2 ceiling; `None` when unlimited or off Linux.
pub fn process_memory_limit() -> Option<u64> {
    imp::memory_limit()
}

#[cfg(unix)]
mod cpu {
    use std::time::Duration;

    use super::ProcessCpu;

    pub(super) fn sample() -> ProcessCpu {
        let self_times = rusage_times(libc::RUSAGE_SELF);
        ProcessCpu {
            self_time: self_times.map(|(user, system)| user + system),
            self_user_time: self_times.map(|(user, _)| user),
            self_system_time: self_times.map(|(_, system)| system),
            children_time: rusage_times(libc::RUSAGE_CHILDREN).map(|(user, system)| user + system),
        }
    }

    /// Cumulative (user, system) CPU time for `who`.
    fn rusage_times(who: libc::c_int) -> Option<(Duration, Duration)> {
        // SAFETY: the all-zero bit pattern is a valid `rusage`, and
        // `getrusage` writes only within the struct it is handed.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `usage` is a properly sized and aligned out-pointer, and
        // self or reaped-children queries need no privileges.
        if unsafe { libc::getrusage(who, &mut usage) } != 0 {
            return None;
        }
        let to_duration = |tv: libc::timeval| -> Option<Duration> {
            Some(
                Duration::from_secs(u64::try_from(tv.tv_sec).ok()?)
                    + Duration::from_micros(u64::try_from(tv.tv_usec).ok()?),
            )
        };
        Some((to_duration(usage.ru_utime)?, to_duration(usage.ru_stime)?))
    }
}

#[cfg(not(unix))]
mod cpu {
    use super::ProcessCpu;

    pub(super) fn sample() -> ProcessCpu {
        ProcessCpu::default()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::ProcessResources;

    // Hand-rolled `task_vm_info` prefix through `phys_footprint` (the kernel
    // accepts any count ≤ the current struct revision; passing the prefix
    // count returns exactly these fields). Layout per XNU osfmk/mach/task_info.h.
    // (libc has no mach `task_vm_info`; its libproc bindings below are used
    // for the thread count.)
    #[repr(C)]
    #[derive(Default)]
    struct TaskVmInfoPrefix {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }

    const TASK_VM_INFO: u32 = 22;
    // mach natural_t (u32) units.
    const PREFIX_COUNT: u32 = (size_of::<TaskVmInfoPrefix>() / size_of::<u32>()) as u32;

    unsafe extern "C" {
        // libSystem: the calling task's control port and task_info(2).
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: u32, info: *mut u8, count: *mut u32) -> i32;
    }

    /// # Safety
    /// `T` must be the plain-integer kernel struct matching `flavor` (the
    /// all-zero bit pattern must be a valid `T`).
    unsafe fn proc_pidinfo_self<T>(flavor: libc::c_int) -> Option<T> {
        // SAFETY: the caller guarantees all-zero is a valid `T`.
        let mut info: T = unsafe { std::mem::zeroed() };
        let size = size_of::<T>() as i32;
        // SAFETY: `info` is a properly sized/aligned out-buffer and
        // `buffersize` tells the kernel its length; self-pid lookups need no
        // extra privileges.
        let filled =
            unsafe { libc::proc_pidinfo(libc::getpid(), flavor, 0, (&raw mut info).cast(), size) };
        (filled == size).then_some(info)
    }

    fn thread_count() -> Option<u64> {
        // SAFETY: `proc_taskinfo` is all integer fields.
        let info: libc::proc_taskinfo = unsafe { proc_pidinfo_self(libc::PROC_PIDTASKINFO) }?;
        u64::try_from(info.pti_threadnum).ok()
    }

    pub(super) fn memory_limit() -> Option<u64> {
        None
    }

    pub(super) fn start_time() -> Option<std::time::SystemTime> {
        // SAFETY: `proc_bsdinfo` is all integer fields.
        let info: libc::proc_bsdinfo = unsafe { proc_pidinfo_self(libc::PROC_PIDTBSDINFO) }?;
        Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(info.pbi_start_tvsec))
    }

    pub(super) fn sample() -> ProcessResources {
        sample_memory()
    }

    pub(super) fn sample_memory() -> ProcessResources {
        let mut info = TaskVmInfoPrefix::default();
        let mut count = PREFIX_COUNT;
        // SAFETY: `info` is a properly sized/aligned out-buffer and `count`
        // tells the kernel its length in natural_t units; TASK_VM_INFO on
        // the caller's own task port cannot fault.
        let kr = unsafe {
            task_info(
                mach_task_self_,
                TASK_VM_INFO,
                (&raw mut info).cast::<u8>(),
                &raw mut count,
            )
        };
        if kr != 0 {
            return ProcessResources::default();
        }
        ProcessResources {
            rss_bytes: Some(info.resident_size),
            peak_rss_bytes: Some(info.resident_size_peak),
            footprint_bytes: Some(info.phys_footprint),
            threads: thread_count(),
            open_files: None,
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::ProcessResources;

    pub(super) fn sample() -> ProcessResources {
        ProcessResources {
            open_files: count_entries("/proc/self/fd"),
            ..sample_memory()
        }
    }

    pub(super) fn sample_memory() -> ProcessResources {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        ProcessResources {
            // /proc/self/status reports VmRSS/VmHWM in kB.
            rss_bytes: field_u64(&status, "VmRSS:").map(|kb| kb * 1024),
            peak_rss_bytes: field_u64(&status, "VmHWM:").map(|kb| kb * 1024),
            footprint_bytes: None,
            // Same file read; no extra cost on the timer path.
            threads: field_u64(&status, "Threads:"),
            open_files: None,
        }
    }

    fn field_u64(status: &str, prefix: &str) -> Option<u64> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|n| n.parse().ok())
    }

    /// The read's own transient descriptor closes with the iterator, so
    /// repeated samples stay comparable.
    fn count_entries(dir: &str) -> Option<u64> {
        Some(std::fs::read_dir(dir).ok()?.count() as u64)
    }

    pub(super) fn memory_limit() -> Option<u64> {
        // cgroup v2 unified hierarchy line: "0::<path>".
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
        let raw = std::fs::read_to_string(format!("/sys/fs/cgroup{path}/memory.max")).ok()?;
        raw.trim().parse().ok()
    }

    pub(super) fn start_time() -> Option<std::time::SystemTime> {
        // Field 22 of /proc/self/stat, in ticks since boot; parse after the
        // parenthesized comm, which may itself contain spaces.
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let after_comm = stat.get(stat.rfind(')')? + 1..)?;
        let start_ticks: u64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
        // SAFETY: sysconf with a valid name reads no memory.
        let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks_per_sec <= 0 {
            return None;
        }
        let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
        let boot_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
        let age_secs = boot_secs - start_ticks as f64 / ticks_per_sec as f64;
        std::time::SystemTime::now()
            .checked_sub(std::time::Duration::try_from_secs_f64(age_secs).ok()?)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::ProcessResources;

    pub(super) fn sample() -> ProcessResources {
        ProcessResources::default()
    }

    pub(super) fn sample_memory() -> ProcessResources {
        ProcessResources::default()
    }

    pub(super) fn start_time() -> Option<std::time::SystemTime> {
        None
    }

    pub(super) fn memory_limit() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        process_start_time, sample_process_cpu, sample_process_memory, sample_process_resources,
    };

    #[test]
    fn a_running_process_reports_its_own_gauges() {
        let usage = sample_process_resources();

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let rss = usage.rss_bytes.expect("rss");
            assert!(rss > 0);
            assert!(
                usage.peak_rss_bytes.expect("peak") >= rss,
                "peak is a high-water mark, so it can never trail current"
            );
            assert!(usage.threads.expect("threads") >= 1);
        }

        #[cfg(target_os = "macos")]
        {
            assert!(usage.footprint_bytes.expect("footprint") > 0);
            assert_eq!(usage.open_files, None);
        }

        #[cfg(target_os = "linux")]
        assert!(usage.open_files.expect("open files") >= 1);

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        assert_eq!((usage.threads, usage.open_files), (None, None));
    }

    /// The reason this function exists is the scan it skips, so a rewrite
    /// that delegated to the full sampler would pass everything but this.
    #[test]
    fn the_memory_only_sample_leaves_open_files_unset() {
        let usage = sample_process_memory();

        assert_eq!(usage.open_files, None);

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert!(usage.rss_bytes.expect("rss") > 0);
    }

    #[test]
    fn cpu_and_start_time_readers_report_this_process() {
        let cpu = sample_process_cpu();
        #[cfg(unix)]
        {
            assert!(
                cpu.self_time.expect("self cpu readable") > std::time::Duration::ZERO,
                "a running test binary has burned some cpu"
            );
            assert_eq!(
                cpu.self_user_time.expect("user split readable")
                    + cpu.self_system_time.expect("system split readable"),
                cpu.self_time.unwrap(),
                "the split fields must sum to the total, same reading"
            );
        }
        #[cfg(not(unix))]
        assert_eq!((cpu.self_time, cpu.children_time), (None, None));

        if let Some(limit) = super::process_memory_limit() {
            assert!(limit > 0, "a present cgroup ceiling is a real byte count");
        }

        let start = process_start_time();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let start = start.expect("start time readable");
            let now = std::time::SystemTime::now();
            assert!(
                start <= now + std::time::Duration::from_secs(1),
                "derived start must not land in the future beyond tick rounding"
            );
            assert!(
                start >= now - std::time::Duration::from_secs(60 * 60),
                "the test binary started within the hour"
            );
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        assert_eq!(start, None);
    }

    /// The thread gauge tracks live threads, not a plausible constant:
    /// parking N new threads raises the sampled count by at least N.
    /// Unrelated tests in this binary start and stop threads concurrently,
    /// so one attempt can under-observe; the invariant must hold within a
    /// few tries.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn thread_gauge_tracks_spawned_threads() {
        use std::sync::mpsc::channel;

        const SPAWNED: u64 = 4;
        let mut deltas = Vec::new();
        for _ in 0..5 {
            let before = sample_process_memory().threads.expect("threads");

            let (stop_tx, stop_rx) = channel::<()>();
            let (ready_tx, ready_rx) = channel::<()>();
            let stop_rx = std::sync::Arc::new(std::sync::Mutex::new(stop_rx));
            let handles: Vec<_> = (0..SPAWNED)
                .map(|_| {
                    let ready = ready_tx.clone();
                    let stop = stop_rx.clone();
                    std::thread::spawn(move || {
                        ready.send(()).ok();
                        // Release the sender before parking: with the
                        // parent's copy dropped too, a thread that dies
                        // before signaling closes the channel and errors
                        // the recv below instead of blocking it.
                        drop(ready);
                        // Park until released (the lock serializes the
                        // recvs; each thread consumes one stop token).
                        let guard = stop.lock().unwrap_or_else(|p| p.into_inner());
                        guard.recv().ok();
                    })
                })
                .collect();
            // The parent holds no sender past this point, so a thread dying
            // before it signals errors the recv instead of blocking it.
            drop(ready_tx);
            for _ in 0..SPAWNED {
                ready_rx.recv().expect("spawned thread ready");
            }

            let during = sample_process_memory().threads.expect("threads");

            for _ in 0..SPAWNED {
                stop_tx.send(()).ok();
            }
            for handle in handles {
                let _ = handle.join();
            }

            if during >= before + SPAWNED {
                return;
            }
            deltas.push(during as i64 - before as i64);
        }
        panic!(
            "thread gauge never observed the {SPAWNED} parked threads \
             (deltas across attempts: {deltas:?})"
        );
    }
}
