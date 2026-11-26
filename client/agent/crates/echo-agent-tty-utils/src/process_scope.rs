//! Kill-handle for a session's child-process trees (`Send + Sync`).
//!
//! If a session's worker thread wedges, its `Drop`s never run and child
//! processes leak. A `ProcessScope` lives outside the worker so a supervisor
//! can `kill_all()` one session's children without restarting the host.
//!
//! # Weak-keyed registry (PID-reuse safety)
//!
//! Each child enrolls as a [`ProcessGroup`]. The scope holds only `Weak`
//! references; the spawn-site owner holds the strong `Arc`. On `kill_all`,
//! only groups whose owner is still alive (i.e. un-reaped children) upgrade
//! successfully — reaped children upgrade to `None` and are skipped. This
//! prevents `killpg` from hitting a reused PID.
//!
//! # Residual
//!
//! `kill_all` SIGKILLs but doesn't `wait` (would race the live owner).
//! A wedged owner's killed leader stays as a zombie until the host exits —
//! bounded by wedge events, not sessions.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use crate::{HANGUP_GRACE, ProcessGroup, new_process_group};

/// A `Send + Sync` kill-handle for one unit's child-process trees. Cheap to
/// clone (shares one inner via `Arc`). See the [module docs](self) for the
/// ownership model and PID-reuse safety argument.
#[derive(Clone)]
pub struct ProcessScope {
    inner: Arc<ScopeInner>,
}

struct ScopeInner {
    /// One `Weak` per enrolled child tree. The strong `Arc` lives at the spawn
    /// site for as long as that child is alive/owned; a dead `Weak` means the
    /// owner already reaped+dropped it, so killing it is neither needed nor safe.
    groups: Mutex<Vec<Weak<ProcessGroup>>>,
    /// Latched once [`kill_all`](ProcessScope::kill_all) has run. Nothing calls
    /// `kill_all` twice (the supervisor drops its handle after), so a child
    /// enrolled *after* close — a spawn that won the close/spawn race on a
    /// wedged actor — would otherwise never be reaped.
    /// [`register`](ProcessScope::register) checks this under the `groups` lock
    /// and kills such a child on the spot instead of enrolling a leaked `Weak`.
    closed: AtomicBool,
}

impl ProcessScope {
    /// Create an empty scope. Infallible — group/job handles are created lazily
    /// as children are enrolled.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ScopeInner {
                groups: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// True once [`kill_all`](Self::kill_all) has run. Enrollment sites use
    /// this to re-check after work done between a successful [`register`] and
    /// publishing the child (e.g. installing a client): a `kill_all` in that
    /// window has already SIGKILLed the enrolled child, so publishing it would
    /// advertise a dead server. The check is racy by nature (close can land
    /// right after it) — it narrows the window; the child itself is reaped by
    /// `kill_all` in every ordering once registered.
    ///
    /// [`register`]: Self::register
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Relaxed)
    }

    /// Configure `cmd` so its spawned child becomes the leader of a new process
    /// group / job. Call this **before** `cmd.spawn()`, then [`enroll`] the
    /// resulting child (or build the group yourself and [`register`] it).
    ///
    /// [`enroll`]: Self::enroll
    /// [`register`]: Self::register
    pub fn prepare(&self, cmd: &mut tokio::process::Command) {
        new_process_group(cmd);
    }

    /// Register an already-built process group so the scope can reap it if its
    /// owner wedges. The scope keeps only a [`Weak`]; **the caller MUST keep the
    /// `Arc` alive** for as long as the child is its responsibility. Dropping the
    /// last `Arc` (clean reap) makes this registration a silent no-op, which is
    /// what keeps [`kill_all`] PID-reuse-safe.
    ///
    /// Returns `true` if the group was enrolled. Returns `false` if the scope
    /// was already closed ([`kill_all`] latched): the caller should treat the
    /// child as dead and fail its start rather than proceed against it.
    ///
    /// A group is killed here unless it wants a hangup first, which needs a
    /// grace this lock cannot afford; [`enroll_terminal_pid`] is the only way to
    /// mark one and reaps it itself. Do not pair
    /// [`ProcessGroup::hang_up_before_kill`] with a bare `register`: nothing
    /// would reap the child.
    ///
    /// [`enroll_terminal_pid`]: Self::enroll_terminal_pid
    ///
    /// For spawn sites (e.g. the MCP child handle, the bash terminal) that
    /// already build their own [`ProcessGroup`] and own its lifecycle; sites that
    /// don't can use [`enroll`] instead.
    ///
    /// [`kill_all`]: Self::kill_all
    /// [`enroll`]: Self::enroll
    pub fn register(&self, group: &Arc<ProcessGroup>) -> bool {
        let mut groups = self.lock();
        if self.inner.closed.load(Ordering::Relaxed) {
            // The scope was already reclaimed (`kill_all` ran) and won't run
            // again, so reap this just-spawned child now rather than enroll a
            // `Weak` that would leak. Closes the close/spawn race where a
            // (possibly wedged) actor's spawn lands after teardown. `killpg` is
            // non-blocking, so killing under the lock is fine and serializes
            // with a concurrent `kill_all`. A shell that needs a hangup first is
            // left for the caller, which can afford the grace outside this lock.
            if !group.wants_hangup() {
                let _ = group.kill();
            }
            return false;
        }
        groups.retain(|w| w.strong_count() > 0);
        groups.push(Arc::downgrade(group));
        true
    }

    /// Build a process group for an already-spawned `child` (which must have been
    /// configured via [`prepare`]), register a [`Weak`] into this scope, and
    /// return the owning `Arc` for the caller to hold. Errors if the child
    /// already exited (nothing to attach) — not a leak — or if the scope was
    /// already closed, in which case the child has been killed and the caller
    /// must not proceed with it.
    ///
    /// [`prepare`]: Self::prepare
    #[must_use = "the returned Arc<ProcessGroup> must be kept alive or the scope cannot reap the child"]
    pub fn enroll(&self, child: &tokio::process::Child) -> io::Result<Arc<ProcessGroup>> {
        let mut group = ProcessGroup::new()?;
        group.attach(child)?;
        self.register_owned(group)
    }

    /// Register a detached synchronous child and return its process-group owner.
    ///
    /// The child must be (or lead) its own group/job — spawn via
    /// [`crate::detach_std_command`] (Unix `setsid`) first, otherwise later
    /// `kill` signals a group that is not this child's.
    ///
    /// The scope holds only a `Weak` registration.
    /// Keep the owner until direct-child reap, then drop it to prevent later
    /// signals to a recycled PID. If the scope is closed, the group is killed
    /// but the caller must still reap the child.
    ///
    /// # Errors
    ///
    /// Returns group creation/attachment errors or a closed-scope error.
    #[must_use = "the returned Arc<ProcessGroup> must be kept alive until the child is reaped"]
    pub fn enroll_std(&self, child: &std::process::Child) -> io::Result<Arc<ProcessGroup>> {
        let mut group = ProcessGroup::new()?;
        group.attach_std(child)?;
        self.register_owned(group)
    }

    fn register_owned(&self, group: ProcessGroup) -> io::Result<Arc<ProcessGroup>> {
        let group = Arc::new(group);
        if !self.register(&group) {
            return Err(io::Error::other(
                "process scope already closed; child killed",
            ));
        }
        Ok(group)
    }

    /// Enroll a shell this process did not spawn through a
    /// [`tokio::process::Command`]. Teardown gives it [`HANGUP_GRACE`] to hang
    /// up before the kill.
    #[must_use = "the returned Arc<ProcessGroup> must be kept alive or the scope cannot reap the child"]
    pub fn enroll_terminal_pid(&self, pid: u32) -> io::Result<Arc<ProcessGroup>> {
        let mut group = ProcessGroup::new()?;
        group.attach_pid(pid)?;
        group.hang_up_before_kill();
        let group = Arc::new(group);
        if !self.register(&group) {
            // Lost the close/spawn race; reap in the same order teardown uses.
            reap_groups(&[Arc::downgrade(&group)]);
            return Err(io::Error::other(
                "process scope already closed; child killed",
            ));
        }
        Ok(group)
    }

    /// Convenience for simple sites: [`prepare`] + spawn + [`enroll`]. Returns
    /// the child together with the owning `Arc<ProcessGroup>`, which the caller
    /// must keep alive for the scope to be able to reap the child.
    ///
    /// [`prepare`]: Self::prepare
    /// [`enroll`]: Self::enroll
    #[must_use = "the returned Arc<ProcessGroup> must be kept alive or the scope cannot reap the child"]
    pub fn spawn(
        &self,
        mut cmd: tokio::process::Command,
    ) -> io::Result<(tokio::process::Child, Arc<ProcessGroup>)> {
        self.prepare(&mut cmd);
        #[allow(clippy::disallowed_methods)]
        // ProcessScope::spawn is the enrollment primitive itself.
        let child = cmd.spawn()?;
        let group = self.enroll(&child)?;
        Ok((child, group))
    }

    /// Idempotently kill every still-owned process tree (`killpg(SIGKILL)` /
    /// `TerminateJobObject`). Safe to call multiple times and from any thread.
    /// Groups whose owner already reaped+dropped them upgrade to `None` and are
    /// skipped — so this never `killpg`s a reused PID.
    pub fn kill_all(&self) {
        let enrolled = {
            let mut groups = self.lock();
            let enrolled = std::mem::take(&mut *groups);
            // Latch closed under the lock: a concurrent `register` either already
            // pushed (its group is in `enrolled` and dies below) or now sees
            // `closed` and kills its own child — nothing slips past teardown.
            self.inner.closed.store(true, Ordering::Relaxed);
            enrolled
        };
        reap_groups(&enrolled);
    }

    /// Lock the group set, tolerating a poisoned mutex: the critical sections
    /// here are panic-free, and a best-effort reaper must still run even if some
    /// unrelated thread panicked while holding the lock.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Weak<ProcessGroup>>> {
        self.inner
            .groups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Number of still-live enrolled groups: weaks whose owning `Arc` has not
    /// yet been dropped. This is exactly the set `kill_all` would `killpg`, so a
    /// spawn-site owner that has reaped its child (and dropped its `Arc`) is no
    /// longer counted — letting callers assert PID-reuse safety.
    pub fn live_count(&self) -> usize {
        self.lock().iter().filter(|w| w.strong_count() > 0).count()
    }
}

impl Default for ProcessScope {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-global [`ProcessScope`] for reaping detached child trees at process
/// exit. Spawn sites (e.g. the local terminal backend) enroll their children
/// here, and the TUI exit paths call [`ProcessScope::kill_all`] on it so a
/// `setsid`-detached background command cannot outlive the process that started
/// it. `kill_all` latches the scope closed, so call it only when the process is
/// genuinely exiting.
pub fn global_process_scope() -> &'static ProcessScope {
    static GLOBAL: OnceLock<ProcessScope> = OnceLock::new();
    GLOBAL.get_or_init(ProcessScope::new)
}

impl Drop for ScopeInner {
    fn drop(&mut self) {
        // RAII backstop: if the last scope handle drops without an explicit
        // `kill_all`, still reap any group whose owner is alive (a wedged unit),
        // in the same order.
        let groups = self.groups.lock().unwrap_or_else(PoisonError::into_inner);
        reap_groups(&groups);
    }
}

/// Hang up the groups that asked for it, one shared grace, then kill.
///
/// A failed signal means the group already exited (ESRCH), which is benign and
/// not actionable at this layer.
fn reap_groups(enrolled: &[Weak<ProcessGroup>]) {
    let mut hung_up = false;
    for group in enrolled.iter().filter_map(Weak::upgrade) {
        if group.wants_hangup() {
            let _ = group.hangup();
            hung_up = true;
        }
    }
    if hung_up {
        std::thread::sleep(HANGUP_GRACE);
    }
    // Upgrade again rather than holding the `Arc`s across the grace: an owner
    // that reaped its child meanwhile has dropped its group, and its pgid may
    // already belong to someone else.
    for group in enrolled.iter().filter_map(Weak::upgrade) {
        let _ = group.kill();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sleeper() -> tokio::process::Command {
        let mut c = tokio::process::Command::new("sleep");
        c.arg("1000");
        c
    }

    /// `wait()` completing == the process actually died (and is reaped). If the
    /// kill failed, the `sleep 1000` would run on and `wait()` would time out.
    async fn died(child: &mut tokio::process::Child) -> bool {
        tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn kill_all_reaps_every_enrolled_child() {
        let scope = ProcessScope::new();
        // The owner (here, the test) keeps the Arcs alive — as a live spawn site
        // would for as long as the child is running.
        let (mut c1, _g1) = scope.spawn(sleeper()).unwrap();
        let (mut c2, _g2) = scope.spawn(sleeper()).unwrap();
        assert_eq!(scope.live_count(), 2);

        scope.kill_all();
        assert!(died(&mut c1).await, "child 1 must die after kill_all");
        assert!(died(&mut c2).await, "child 2 must die after kill_all");
    }

    #[tokio::test]
    async fn kill_all_is_idempotent() {
        let scope = ProcessScope::new();
        let (mut c, _g) = scope.spawn(sleeper()).unwrap();
        scope.kill_all();
        scope.kill_all(); // second call must not panic / error
        assert!(died(&mut c).await);
    }

    /// PID-reuse safety: once the owner drops its `Arc` (simulating a clean
    /// reap), the scope no longer references the group and `kill_all` is a no-op
    /// for it — it must NOT `killpg` a now-reapable/reused group id.
    #[tokio::test]
    async fn kill_all_skips_group_whose_owner_dropped() {
        let scope = ProcessScope::new();
        let (mut c, group) = scope.spawn(sleeper()).unwrap();
        assert_eq!(scope.live_count(), 1);

        // Owner reaps + releases ownership.
        drop(group);
        assert_eq!(
            scope.live_count(),
            0,
            "dropping the owner's Arc must make the scope's weak dead"
        );

        scope.kill_all(); // must be a no-op for the now-unowned group
        // The child was never killed by the scope; clean it up so the test
        // doesn't leak a real `sleep` process.
        let _ = c.start_kill();
        let _ = c.wait().await;
    }

    #[tokio::test]
    async fn drop_reaps_children_while_owner_alive() {
        let scope = ProcessScope::new();
        // Owner Arc is held by the test, so the scope's weak is live; dropping
        // the scope's last handle must SIGKILL the still-owned group.
        let (mut c, _g) = scope.spawn(sleeper()).unwrap();
        drop(scope);
        assert!(
            died(&mut c).await,
            "dropping the scope must reap a still-owned enrolled child"
        );
    }

    /// Close/spawn race: a child enrolled *after* `kill_all` must be killed on
    /// the spot, not leaked — `kill_all` won't run again to catch it — and the
    /// caller must be told (enroll errors) so it doesn't proceed with a dead child.
    #[tokio::test]
    async fn only_a_terminal_enrollment_asks_for_a_hangup() {
        let scope = ProcessScope::new();
        let mut cmd = sleeper();
        scope.prepare(&mut cmd);
        let (child, group) = scope.spawn(cmd).unwrap();
        let pid = child.id().expect("pid");
        assert!(!group.wants_hangup());

        let terminal = scope.enroll_terminal_pid(pid).unwrap();
        assert!(terminal.wants_hangup());

        scope.kill_all();
    }

    #[tokio::test]
    async fn register_after_kill_all_reaps_immediately() {
        let scope = ProcessScope::new();
        scope.kill_all(); // close the scope

        let mut cmd = sleeper();
        scope.prepare(&mut cmd);
        #[allow(clippy::disallowed_methods)] // test: exercises enroll() after close
        let mut child = cmd.spawn().unwrap();
        assert!(
            scope.enroll(&child).is_err(),
            "post-close enroll must surface the closed scope"
        );
        assert_eq!(
            scope.live_count(),
            0,
            "a post-close register must not enroll the group"
        );
        assert!(
            died(&mut child).await,
            "a child registered after kill_all must be killed immediately"
        );
    }

    fn std_sleeper() -> std::process::Command {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("1000");
        crate::detach_std_command(&mut cmd);
        cmd
    }

    #[test]
    fn enroll_std_after_close_kills_and_leaves_reap_to_caller() {
        let scope = ProcessScope::new();
        scope.kill_all();
        #[allow(clippy::disallowed_methods)] // test: exercises enroll_std after close
        let mut child = std_sleeper().spawn().expect("spawn sleeper");

        assert!(scope.enroll_std(&child).is_err());
        assert!(child.wait().expect("caller reaps child").code().is_none());
        assert_eq!(scope.live_count(), 0);
    }

    #[test]
    fn enroll_std_owner_accepts_close_after_registration() {
        let scope = ProcessScope::new();
        #[allow(clippy::disallowed_methods)] // test: exercises enroll_std
        let mut child = std_sleeper().spawn().expect("spawn sleeper");
        let group = scope.enroll_std(&child).expect("enroll child");

        scope.kill_all();
        let status = crate::wait_child_bounded(&mut child, Duration::from_secs(3))
            .expect("wait for killed child")
            .expect("killed child reaches terminal status");
        assert!(!status.success());
        drop(group);
        assert_eq!(scope.live_count(), 0);
    }
}
