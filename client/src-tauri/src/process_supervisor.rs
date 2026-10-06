//! Owned process trees. Unix uses process groups; Windows uses Jobs.
use std::{
    io,
    process::ExitStatus,
    time::{Duration, Instant},
};

pub struct AsyncChild {
    inner: Box<dyn process_wrap::tokio::ChildWrapper>,
}
impl std::ops::Deref for AsyncChild {
    type Target = dyn process_wrap::tokio::ChildWrapper;
    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}
impl std::ops::DerefMut for AsyncChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.inner
    }
}
impl AsyncChild {
    #[cfg(windows)]
    fn kill_tree(&mut self) {
        let _ = self.inner.start_kill();
    }
}

pub struct SyncChild {
    inner: Box<dyn process_wrap::std::ChildWrapper>,
    #[cfg(windows)]
    job: windows_job::Job,
}
impl std::ops::Deref for SyncChild {
    type Target = dyn process_wrap::std::ChildWrapper;
    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}
impl std::ops::DerefMut for SyncChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.inner
    }
}
impl SyncChild {
    fn kill_tree(&mut self) {
        #[cfg(windows)]
        {
            let _ = self.job.terminate();
            return;
        }
        #[cfg(not(windows))]
        let _ = self.inner.start_kill();
    }
}
impl Drop for SyncChild {
    fn drop(&mut self) {
        self.kill_tree();
        let _ = self.inner.try_wait();
    }
}
const GRACE: Duration = Duration::from_millis(500);
const REAP: Duration = Duration::from_secs(2);
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;
#[cfg(windows)]
pub(crate) const WINDOWLESS_NODE_FLAGS: u32 = CREATE_NO_WINDOW | DETACHED_PROCESS;

/// Run a non-interactive CLI tool without opening a console from the desktop app.
/// Interactive shells use a PTY and must not use this helper.
pub(crate) fn background_sync_command(
    executable: impl AsRef<std::ffi::OsStr>,
) -> std::process::Command {
    let command = std::process::Command::new(executable);
    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt;
        let mut command = command;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    command
}

pub(crate) fn background_async_command(
    executable: impl AsRef<std::ffi::OsStr>,
) -> tokio::process::Command {
    let command = tokio::process::Command::new(executable);
    #[cfg(windows)]
    let command = {
        let mut command = command;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    command
}

#[cfg(windows)]
mod windows_job {
    use std::{
        io, ptr,
        sync::{Mutex, MutexGuard, OnceLock},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
            },
            Threading::{
                GetCurrentProcess, GetProcessId, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
            },
        },
    };

    pub(super) struct Job(HANDLE);
    // Kernel Job handles are safe to share; only this wrapper owns and closes each handle.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        fn new() -> io::Result<Self> {
            let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(handle);
            job.set_silent_breakaway(false)?;
            Ok(job)
        }

        pub(super) fn new_child() -> io::Result<Self> {
            let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(handle);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            // No breakaway permission: Node, its IPC workers, and their descendants
            // remain in this job even when libuv uses DETACHED_PROCESS.
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        fn set_silent_breakaway(&self, enabled: bool) -> io::Result<()> {
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_BREAKAWAY_OK
                | if enabled {
                    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK
                } else {
                    0
                };
            let ok = unsafe {
                SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn assign(&self, process: HANDLE) -> io::Result<()> {
            if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    /// The std::process API exposes the process handle but not its primary
    /// thread handle. A suspended child has not run any application code yet.
    pub(super) fn resume_child(process: HANDLE) -> io::Result<()> {
        let pid = unsafe { GetProcessId(process) };
        if pid == 0 {
            return Err(io::Error::last_os_error());
        }
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        struct Snapshot(HANDLE);
        impl Drop for Snapshot {
            fn drop(&mut self) {
                unsafe { CloseHandle(self.0) };
            }
        }
        let _snapshot = Snapshot(snapshot);
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut found = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while found {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let result = unsafe { ResumeThread(thread) };
                unsafe { CloseHandle(thread) };
                if result == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        Err(io::Error::other("suspended child thread was not found"))
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    static HOST_JOB: OnceLock<Option<Job>> = OnceLock::new();
    static LAUNCH_LOCK: Mutex<bool> = Mutex::new(false);

    pub(super) fn install_host() {
        HOST_JOB.get_or_init(|| {
            let result = Job::new().and_then(|job| {
                job.assign(unsafe { GetCurrentProcess() })?;
                Ok(job)
            });
            match result {
                Ok(job) => Some(job),
                Err(error) => {
                    tracing::warn!(%error, "could not install Windows host Job");
                    None
                }
            }
        });
    }

    fn host() -> Option<&'static Job> {
        install_host();
        HOST_JOB.get().and_then(Option::as_ref)
    }

    // Tauri restarts and launches its installer without CREATE_BREAKAWAY_FROM_JOB.
    pub(super) fn permit_relaunch() -> io::Result<()> {
        let mut silent = LAUNCH_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*silent {
            if let Some(job) = host() {
                job.set_silent_breakaway(true)?;
            }
            *silent = true;
        }
        Ok(())
    }

    struct InstallerLaunchGuard {
        job: Option<&'static Job>,
        previous_silent: bool,
        lock: MutexGuard<'static, bool>,
    }

    impl Drop for InstallerLaunchGuard {
        fn drop(&mut self) {
            if let Some(job) = self.job {
                if let Err(error) = job.set_silent_breakaway(self.previous_silent) {
                    tracing::error!(%error, "could not restore Windows host Job limits");
                    return;
                }
            }
            *self.lock = self.previous_silent;
        }
    }

    pub(super) fn with_installer_launch<T>(install: impl FnOnce() -> T) -> io::Result<T> {
        let mut lock = LAUNCH_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous_silent = *lock;
        let job = host();
        if !previous_silent {
            if let Some(job) = job {
                job.set_silent_breakaway(true)?;
            }
            *lock = true;
        }
        let _guard = InstallerLaunchGuard {
            job,
            previous_silent,
            lock,
        };
        Ok(install())
    }
}

#[cfg(windows)]
pub fn install_host_job() {
    windows_job::install_host();
}

#[cfg(not(windows))]
pub fn install_host_job() {}

pub fn permit_application_relaunch() -> io::Result<()> {
    #[cfg(windows)]
    windows_job::permit_relaunch()?;
    Ok(())
}

pub fn with_external_installer_launch<T>(install: impl FnOnce() -> T) -> io::Result<T> {
    #[cfg(windows)]
    return windows_job::with_installer_launch(install);
    #[cfg(not(windows))]
    Ok(install())
}

pub fn spawn_async(command: tokio::process::Command) -> io::Result<AsyncChild> {
    use process_wrap::tokio::*;
    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    wrapped.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    {
        windows_job::install_host();
        let mut flags = CreationFlags(Default::default());
        flags.0 .0 = CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB;
        wrapped.wrap(flags).wrap(JobObject);
    }
    let inner = wrapped.wrap(KillOnDrop).spawn()?;
    Ok(AsyncChild { inner })
}

pub fn spawn_sync(command: std::process::Command) -> io::Result<SyncChild> {
    #[cfg(windows)]
    {
        use std::os::{windows::io::AsRawHandle, windows::process::CommandExt};
        windows_job::install_host();
        let mut command = command;
        // Suspend before assignment so an early IPC fork cannot escape the job.
        // CREATE_NO_WINDOW is ignored with DETACHED_PROCESS, which gives Node
        // no inherited console at all.
        command
            .creation_flags(WINDOWLESS_NODE_FLAGS | CREATE_BREAKAWAY_FROM_JOB | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let handle = child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let job = match windows_job::Job::new_child().and_then(|job| {
            job.assign(handle)?;
            Ok(job)
        }) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if let Err(error) = windows_job::resume_child(handle) {
            drop(job);
            let _ = child.wait();
            return Err(error);
        }
        return Ok(SyncChild {
            inner: Box::new(child),
            job,
        });
    }
    #[cfg(not(windows))]
    {
        use process_wrap::std::*;
        let mut wrapped = CommandWrap::from(command);
        wrapped.wrap(ProcessGroup::leader());
        let inner = wrapped.spawn()?;
        Ok(SyncChild { inner })
    }
}

pub async fn stop_async(child: &mut AsyncChild) -> Option<ExitStatus> {
    #[cfg(unix)]
    let _ = child.signal(libc::SIGTERM);
    #[cfg(windows)]
    child.kill_tree();
    let status = tokio::time::timeout(GRACE, child.wait())
        .await
        .ok()
        .and_then(Result::ok);
    // The shell may have exited while descendants still own output pipes.
    let _ = child.start_kill();
    tokio::time::timeout(REAP, child.wait())
        .await
        .ok()
        .and_then(Result::ok)
        .or(status)
}

/// Call after requesting application-level shutdown; force cleanup is bounded.
pub fn stop_sync(child: &mut SyncChild) {
    #[cfg(unix)]
    let _ = child.signal(libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    child.kill_tree();
    let deadline = Instant::now() + REAP;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    tracing::warn!("process tree did not finish within shutdown deadline");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stop_reclaims_descendants_and_their_pipes() {
        use tokio::io::AsyncReadExt;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late-write");
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("(sleep 1; echo late > \"$1\") & echo ready; wait")
            .arg("sh")
            .arg(&marker)
            .stdout(std::process::Stdio::piped());
        let mut child = spawn_async(command).unwrap();
        let mut stdout = child.stdout().take().unwrap();
        let mut ready = [0; 6];
        stdout.read_exact(&mut ready).await.unwrap();
        stop_async(&mut child).await;
        let mut rest = Vec::new();
        tokio::time::timeout(REAP, stdout.read_to_end(&mut rest))
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn spawn_delayed_marker(marker: &str) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_supervisor::windows_tests::tree_helper",
                "--nocapture",
            ])
            .env("ECHO_SUPERVISOR_HELPER", "deferred_grandchild")
            .env("ECHO_SUPERVISOR_MARKER", marker)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    fn wait_for_marker_ready(marker: &str) {
        let ready = format!("{marker}.ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::path::Path::new(&ready).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(std::path::Path::new(&ready).exists());
    }

    #[test]
    fn tree_helper() {
        let Ok(mode) = std::env::var("ECHO_SUPERVISOR_HELPER") else {
            return;
        };
        let marker = std::env::var("ECHO_SUPERVISOR_MARKER").unwrap();
        if mode == "background_cli" {
            let console = unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() };
            let visible = !console.is_null()
                && unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(console) }
                    != 0;
            std::fs::write(marker, if visible { "visible" } else { "hidden" }).unwrap();
        } else if mode == "grandchild" || mode == "deferred_grandchild" {
            std::fs::write(format!("{marker}.ready"), b"ready").unwrap();
            if mode == "deferred_grandchild" {
                let go = format!("{marker}.go");
                let deadline = Instant::now() + Duration::from_secs(30);
                while !std::path::Path::new(&go).exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(std::path::Path::new(&go).exists());
            } else {
                std::thread::sleep(Duration::from_secs(1));
            }
            std::fs::write(marker, b"late").unwrap();
        } else if mode == "owned_job_owner" {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "process_supervisor::windows_tests::tree_helper",
                    "--nocapture",
                ])
                .env("ECHO_SUPERVISOR_HELPER", "detached_parent")
                .env("ECHO_SUPERVISOR_MARKER", &marker)
                .stdout(std::process::Stdio::null());
            let _child = spawn_sync(command).unwrap();
            std::fs::write(format!("{marker}.owner_ready"), b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        } else {
            if mode == "detached_parent" {
                assert!(
                    unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null()
                );
            }
            if mode == "parent" {
                let go = format!("{marker}.go");
                for _ in 0..500 {
                    if std::path::Path::new(&go).exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(std::path::Path::new(&go).exists());
            }
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process_supervisor::windows_tests::tree_helper",
                    "--nocapture",
                ])
                .env(
                    "ECHO_SUPERVISOR_HELPER",
                    if mode == "parent" || mode == "eager_parent" {
                        "worker"
                    } else if mode == "detached_parent" {
                        "deferred_grandchild"
                    } else {
                        "grandchild"
                    },
                )
                .spawn()
                .unwrap();
            let _ = child.wait();
        }
    }

    #[tokio::test]
    async fn background_cli_commands_have_no_visible_console() {
        let dir = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        for asynchronous in [false, true] {
            let marker = dir.path().join(if asynchronous { "async" } else { "sync" });
            let args = [
                "--exact",
                "process_supervisor::windows_tests::tree_helper",
                "--nocapture",
            ];
            if asynchronous {
                let output = background_async_command(&exe)
                    .args(args)
                    .env("ECHO_SUPERVISOR_HELPER", "background_cli")
                    .env("ECHO_SUPERVISOR_MARKER", &marker)
                    .output()
                    .await
                    .unwrap();
                assert!(output.status.success());
            } else {
                let output = background_sync_command(&exe)
                    .args(args)
                    .env("ECHO_SUPERVISOR_HELPER", "background_cli")
                    .env("ECHO_SUPERVISOR_MARKER", &marker)
                    .output()
                    .unwrap();
                assert!(output.status.success());
            }
            assert_eq!(std::fs::read(marker).unwrap(), b"hidden");
        }
    }
    #[tokio::test]
    async fn windows_job_reclaims_three_levels_and_inherited_pipes() {
        use tokio::io::AsyncReadExt;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_supervisor::windows_tests::tree_helper",
                "--nocapture",
            ])
            .env("ECHO_SUPERVISOR_HELPER", "parent")
            .env("ECHO_SUPERVISOR_MARKER", &marker)
            .stdout(std::process::Stdio::piped());
        let mut child = spawn_async(command).unwrap();
        let mut stdout = child.stdout().take().unwrap();
        std::fs::write(format!("{}.go", marker.display()), b"go").unwrap();
        let ready = dir.path().join("late.ready");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop_async(&mut child).await;
        tokio::time::timeout(REAP, stdout.read_to_end(&mut Vec::new()))
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn windows_job_reclaims_descendants_spawned_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_supervisor::windows_tests::tree_helper",
                "--nocapture",
            ])
            .env("ECHO_SUPERVISOR_HELPER", "eager_parent")
            .env("ECHO_SUPERVISOR_MARKER", &marker)
            .stdout(std::process::Stdio::null());
        let mut child = spawn_async(command).unwrap();
        let ready = dir.path().join("late.ready");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop_async(&mut child).await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!marker.exists());
    }

    #[test]
    fn windows_sync_job_reclaims_descendants_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_supervisor::windows_tests::tree_helper",
                "--nocapture",
            ])
            .env("ECHO_SUPERVISOR_HELPER", "parent")
            .env("ECHO_SUPERVISOR_MARKER", &marker)
            .stdout(std::process::Stdio::null());
        let child = spawn_sync(command).unwrap();
        std::fs::write(format!("{}.go", marker.display()), b"go").unwrap();
        let ready = dir.path().join("late.ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists());
        drop(child);
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists());
    }

    #[test]
    fn windows_sync_job_reclaims_tree_when_owner_is_force_killed() {
        use std::os::windows::process::CommandExt;

        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_supervisor::windows_tests::tree_helper",
                "--nocapture",
            ])
            .env("ECHO_SUPERVISOR_HELPER", "owned_job_owner")
            .env("ECHO_SUPERVISOR_MARKER", &marker)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let owner_ready = format!("{}.owner_ready", marker.display());
        wait_for_marker_ready(&marker.display().to_string());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::path::Path::new(&owner_ready).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(std::path::Path::new(&owner_ready).exists());

        // Do not use /T: only the Job may kill the descendants.
        let killed = std::process::Command::new("taskkill.exe")
            .args(["/PID", &owner.id().to_string(), "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap();
        assert!(killed.success());
        let _ = owner.wait();
        std::fs::write(format!("{}.go", marker.display()), b"go").unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists(), "a Node descendant outlived its owner");
    }

    #[test]
    fn windows_host_job_reclaims_descendants_when_owner_exits() {
        let Ok(marker) = std::env::var("ECHO_HOST_JOB_MARKER") else {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("late");
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process_supervisor::windows_tests::windows_host_job_reclaims_descendants_when_owner_exits",
                    "--nocapture",
                ])
                .env("ECHO_HOST_JOB_MARKER", &marker)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
            std::thread::sleep(Duration::from_millis(1100));
            assert!(!marker.exists());
            return;
        };

        install_host_job();
        let _child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_supervisor::windows_tests::tree_helper",
                "--nocapture",
            ])
            .env("ECHO_SUPERVISOR_HELPER", "grandchild")
            .env("ECHO_SUPERVISOR_MARKER", &marker)
            .spawn()
            .unwrap();
        let ready = format!("{marker}.ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::path::Path::new(&ready).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(std::path::Path::new(&ready).exists());
        std::process::exit(0);
    }

    #[test]
    fn windows_host_job_allows_relaunch_after_owner_exits() {
        let Ok(marker) = std::env::var("ECHO_HOST_JOB_RELAUNCH_MARKER") else {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("relaunched");
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process_supervisor::windows_tests::windows_host_job_allows_relaunch_after_owner_exits",
                    "--nocapture",
                ])
                .env("ECHO_HOST_JOB_RELAUNCH_MARKER", &marker)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
            std::fs::write(format!("{}.go", marker.display()), b"go").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !marker.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(marker.exists());
            return;
        };

        install_host_job();
        permit_application_relaunch().unwrap();
        let failed_install = with_external_installer_launch(|| Err::<(), _>("test failure"));
        assert!(matches!(failed_install, Ok(Err("test failure"))));
        let _child = spawn_delayed_marker(&marker);
        wait_for_marker_ready(&marker);
        std::process::exit(0);
    }

    #[test]
    fn windows_installer_breakaway_restores_host_cleanup_on_error() {
        let Ok(marker) = std::env::var("ECHO_HOST_JOB_INSTALLER_MARKER") else {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("installer");
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process_supervisor::windows_tests::windows_installer_breakaway_restores_host_cleanup_on_error",
                    "--nocapture",
                ])
                .env("ECHO_HOST_JOB_INSTALLER_MARKER", &marker)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
            std::fs::write(
                format!("{}.go", marker.with_extension("escaped").display()),
                b"go",
            )
            .unwrap();
            std::fs::write(
                format!("{}.go", marker.with_extension("attached").display()),
                b"go",
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !marker.with_extension("escaped").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(marker.with_extension("escaped").exists());
            std::thread::sleep(Duration::from_millis(200));
            assert!(!marker.with_extension("attached").exists());
            return;
        };

        install_host_job();
        let escaped = format!("{marker}.escaped");
        let attached = format!("{marker}.attached");
        let _installer = with_external_installer_launch(|| spawn_delayed_marker(&escaped)).unwrap();
        wait_for_marker_ready(&escaped);
        let failed_install = with_external_installer_launch(|| Err::<(), _>("test failure"));
        assert!(matches!(failed_install, Ok(Err("test failure"))));
        let _ordinary = spawn_delayed_marker(&attached);
        wait_for_marker_ready(&attached);
        std::process::exit(0);
    }
}
