//! One physical desktop shared by all sessions. Never held for user approval.
//! Owned guards can travel with native work when its HTTP caller is dropped.
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

#[cfg(any(target_os = "macos", target_os = "windows", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserInput {
    Move,
    Key,
    Button,
    Scroll,
    Drag,
}
struct InputOperation {
    id: u64,
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    run: String,
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    pid: Option<u32>,
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    foreground: bool,
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    token: CancellationToken,
}
#[derive(Default)]
struct InputState {
    operations: Vec<InputOperation>,
    // Keep target identity across model thinking/observation gaps. Otherwise
    // an edit during a gap would disappear after the recent-input interval.
    targets: std::collections::HashSet<(String, u32)>,
    grants: std::collections::HashMap<String, Instant>,
    blocked: std::collections::HashSet<(String, Option<u32>)>,
    recent: std::collections::HashMap<u32, Instant>,
}
static INPUT: std::sync::Mutex<Option<InputState>> = std::sync::Mutex::new(None);
pub struct InputLease(u64);
impl Drop for InputLease {
    fn drop(&mut self) {
        if let Some(state) = INPUT.lock().unwrap().as_mut() {
            state.operations.retain(|operation| operation.id != self.0);
        }
    }
}
/// Only the current action's child token is registered here. Human activity
/// never revokes the task token or stops model/browser work in other apps.
pub fn begin_input(
    run: &str,
    pid: Option<u32>,
    foreground: bool,
    _token: &CancellationToken,
) -> Result<InputLease, String> {
    let mut state = INPUT.lock().unwrap();
    let state = state.get_or_insert_with(Default::default);
    state
        .recent
        .retain(|_, at| at.elapsed() < Duration::from_millis(1200));
    let key = (run.to_owned(), pid);
    if let Some(pid) = pid {
        state.targets.insert((run.to_owned(), pid));
    }
    let grant = state.grants.get(run).copied();
    let recent = |pid: &u32| {
        state
            .recent
            .get(pid)
            .is_some_and(|at| grant.is_none_or(|grant| *at >= grant))
    };
    if state.blocked.contains(&key) || pid.is_some_and(|pid| recent(&pid)) {
        state.blocked.insert(key);
        eprintln!(
            "[computer-input-conflict] run={run} target_pid={pid:?} reason=target_user_input"
        );
        return Err("input_conflict: user input conflicts with this application; no action dispatched. Observe the result, then await a new ordinary user instruction for this target. Other applications may continue.".into());
    }
    if foreground && state.recent.keys().any(recent) {
        // Unrelated app activity is a temporary foreground contention, never a
        // persistent per-run lock. Background operations remain independent.
        eprintln!("[computer-input-conflict] run={run} target_pid={pid:?} reason=desktop_busy");
        return Err("desktop_busy: user is interacting with another application; no foreground input dispatched. Background observations/actions may continue. Do not repeatedly activate windows; explain if foreground access is needed.".into());
    }
    let id = TICKET.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    state.operations.push(InputOperation {
        id,
        #[cfg(any(target_os = "macos", target_os = "windows", test))]
        run: run.to_owned(),
        #[cfg(any(target_os = "macos", target_os = "windows", test))]
        pid,
        #[cfg(any(target_os = "macos", target_os = "windows", test))]
        foreground,
        #[cfg(any(target_os = "macos", target_os = "windows", test))]
        token: _token.clone(),
    });
    Ok(InputLease(id))
}
#[cfg(any(target_os = "macos", test))]
pub fn monitor_lost() {
    if let Some(state) = INPUT.lock().unwrap().as_ref() {
        for operation in &state.operations {
            operation.token.cancel();
        }
    }
}
pub fn start_run(run: &str) {
    INPUT
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .grants
        .insert(run.into(), Instant::now());
}
pub fn finish_run(run: &str) {
    if let Some(state) = INPUT.lock().unwrap().as_mut() {
        state.blocked.retain(|(owner, _)| owner != run);
        state.targets.retain(|(owner, _)| owner != run);
        state.grants.remove(run);
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub fn user_input(pid: Option<u32>, kind: UserInput) {
    if kind == UserInput::Move {
        return;
    }
    let mut state = INPUT.lock().unwrap();
    let state = state.get_or_insert_with(Default::default);
    state
        .recent
        .retain(|_, at| at.elapsed() < Duration::from_millis(1200));
    // Unknown destinations still count as foreground activity, but cannot be
    // used to claim an unrelated background app was edited.
    state.recent.insert(pid.unwrap_or(0), Instant::now());
    if let Some(pid) = pid {
        let owners: Vec<_> = state
            .targets
            .iter()
            .filter(|(_, target)| *target == pid)
            .map(|(run, _)| (run.clone(), Some(pid)))
            .collect();
        for owner in owners {
            if state.blocked.insert(owner.clone()) {
                eprintln!("[computer-input-conflict] run={} target_pid={} user_pid={} input={:?} target_locked=true", owner.0, pid, pid, kind);
            }
        }
    }
    for operation in &state.operations {
        if operation.foreground || (pid.is_some() && operation.pid == pid) {
            if pid.is_some() && operation.pid == pid {
                state.blocked.insert((operation.run.clone(), operation.pid));
            }
            eprintln!("[computer-input-conflict] run={} target_pid={:?} user_pid={:?} input={:?} foreground={} action_cancelled=true",operation.run,operation.pid,pid,kind,operation.foreground);
            operation.token.cancel();
        }
    }
}
static CURRENT: std::sync::Mutex<Option<(u64, CancellationToken)>> = std::sync::Mutex::new(None);
static TICKET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WATCHES: std::sync::Mutex<Option<std::collections::HashMap<String, CancellationToken>>> =
    std::sync::Mutex::new(None);
pub fn watch(task: &str, token: CancellationToken) {
    WATCHES
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(task.into(), token);
}
pub struct DesktopLease {
    _guard: OwnedMutexGuard<()>,
    ticket: u64,
}
#[cfg(test)]
impl DesktopLease {
    pub fn for_test(guard: OwnedMutexGuard<()>) -> Self {
        Self {
            _guard: guard,
            ticket: u64::MAX,
        }
    }
}
impl Drop for DesktopLease {
    fn drop(&mut self) {
        let mut current = CURRENT.lock().unwrap();
        if current
            .as_ref()
            .is_some_and(|(ticket, _)| *ticket == self.ticket)
        {
            current.take();
        }
    }
}
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub fn interrupt_current() -> bool {
    let mut interrupted = false;
    if let Ok(current) = CURRENT.try_lock() {
        if let Some((_, token)) = current.as_ref() {
            token.cancel();
            interrupted = true;
        }
    }
    // The explicit emergency shortcut also revokes tasks between physical
    // calls. Ordinary human input uses only per-action child tokens above.
    if let Ok(watches) = WATCHES.try_lock() {
        if let Some(watches) = watches.as_ref() {
            for token in watches.values() {
                if !token.is_cancelled() {
                    token.cancel();
                    interrupted = true;
                }
            }
        }
    }
    interrupted
}
static OWNERS: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
    std::sync::Mutex::new(None);
pub fn claim(task: &str, target: &str) -> Result<(), String> {
    if target.is_empty() {
        return Err("target_required: no application identity".into());
    }
    let mut owners = OWNERS.lock().unwrap();
    let owners = owners.get_or_insert_with(Default::default);
    if owners.get(target).is_some_and(|owner| owner != task) {
        return Err("application_busy: another computer task owns this application; pause it before continuing".into());
    }
    owners.insert(target.into(), task.into());
    Ok(())
}
pub fn release(task: &str) {
    if let Some(owners) = OWNERS.lock().unwrap().as_mut() {
        owners.retain(|_, owner| owner != task);
    }
    if let Some(watches) = WATCHES.lock().unwrap().as_mut() {
        watches.remove(task);
    }
}
fn desktop() -> &'static Arc<Mutex<()>> {
    static DESKTOP: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
    DESKTOP.get_or_init(|| Arc::new(Mutex::new(())))
}
pub async fn acquire(token: &CancellationToken) -> Result<DesktopLease, String> {
    tokio::select! {
        biased;
        _ = token.cancelled() => Err("cancelled: desktop queue cancelled; no input dispatched".into()),
        guard = desktop().clone().lock_owned() => {
            let ticket=TICKET.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
            *CURRENT.lock().unwrap()=Some((ticket,token.clone()));
            Ok(DesktopLease{_guard:guard,ticket})
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn explicit_emergency_revokes_tasks_between_physical_calls() {
        let _serial = SERIAL.lock().unwrap();
        let token = CancellationToken::new();
        watch("foreground-gap-test", token.clone());
        assert!(interrupt_current());
        assert!(token.is_cancelled());
        release("foreground-gap-test");
    }
    #[test]
    fn losing_input_monitor_cancels_only_registered_actions() {
        let _serial = SERIAL.lock().unwrap();
        *INPUT.lock().unwrap() = None;
        let task = CancellationToken::new();
        let action = task.child_token();
        watch("monitor-loss-test", task.clone());
        let lease = begin_input("monitor-loss-test", Some(180), true, &action).unwrap();
        monitor_lost();
        assert!(action.is_cancelled());
        assert!(!task.is_cancelled());
        drop(lease);
        release("monitor-loss-test");
        *INPUT.lock().unwrap() = None;
    }
    #[test]
    fn human_input_only_cancels_a_conflicting_action_and_never_its_task() {
        let _serial = SERIAL.lock().unwrap();
        *INPUT.lock().unwrap() = None;
        let task = CancellationToken::new();
        watch("background-test", task.clone());
        let a = task.child_token();
        let b = task.child_token();
        let _a = begin_input("run", Some(101), false, &a).unwrap();
        let _b = begin_input("run", Some(102), false, &b).unwrap();
        user_input(Some(101), UserInput::Move);
        assert!(!a.is_cancelled() && !b.is_cancelled());
        user_input(Some(103), UserInput::Key);
        assert!(!a.is_cancelled() && !b.is_cancelled());
        user_input(Some(101), UserInput::Key);
        assert!(a.is_cancelled());
        assert!(!b.is_cancelled() && !task.is_cancelled());
        assert!(begin_input("run", Some(101), false, &task.child_token()).is_err());
        INPUT.lock().unwrap().as_mut().unwrap().recent.clear();
        assert!(begin_input("run", Some(101), false, &task.child_token()).is_err());
        assert!(begin_input("new-user-turn", Some(101), false, &task.child_token()).is_ok());
        assert!(begin_input("run", Some(104), false, &task.child_token()).is_ok());
        user_input(Some(103), UserInput::Key);
        assert!(begin_input("run", None, true, &task.child_token()).is_err());
        release("background-test");
        *INPUT.lock().unwrap() = None;
    }
    #[test]
    fn foreground_contention_cancels_only_current_input_and_stop_reaches_all_children() {
        let _serial = SERIAL.lock().unwrap();
        *INPUT.lock().unwrap() = None;
        let task = CancellationToken::new();
        let action = task.child_token();
        let _lease = begin_input("run", Some(105), true, &action).unwrap();
        user_input(Some(777), UserInput::Key);
        assert!(action.is_cancelled());
        assert!(!task.is_cancelled());
        drop(_lease);
        INPUT.lock().unwrap().as_mut().unwrap().recent.clear();
        assert!(begin_input("run", Some(105), true, &task.child_token()).is_ok());
        assert!(begin_input("run", Some(106), false, &task.child_token()).is_ok());
        let next = task.child_token();
        task.cancel();
        assert!(next.is_cancelled());
        *INPUT.lock().unwrap() = None;
    }
    #[test]
    fn input_during_model_thinking_keeps_the_target_blocked_after_the_quiet_interval() {
        let _serial = SERIAL.lock().unwrap();
        *INPUT.lock().unwrap() = None;
        let task = CancellationToken::new();
        drop(begin_input("thinking-run", Some(180), false, &task.child_token()).unwrap());
        user_input(Some(180), UserInput::Key);
        INPUT.lock().unwrap().as_mut().unwrap().recent.clear();
        assert!(!task.is_cancelled());
        assert!(begin_input("thinking-run", Some(180), false, &task.child_token()).is_err());
        assert!(begin_input("thinking-run", Some(181), false, &task.child_token()).is_ok());
        finish_run("thinking-run");
        assert!(begin_input("ordinary-followup", Some(180), false, &task.child_token()).is_ok());
        *INPUT.lock().unwrap() = None;
    }
    #[test]
    fn unrelated_input_is_transient_and_a_new_user_grant_does_not_inherit_old_events() {
        let _serial = SERIAL.lock().unwrap();
        *INPUT.lock().unwrap() = None;
        let token = CancellationToken::new();
        user_input(Some(900), UserInput::Key);
        start_run("new-grant");
        assert!(begin_input("new-grant", Some(901), true, &token).is_ok());
        user_input(Some(900), UserInput::Button);
        let error = begin_input("new-grant", Some(901), true, &token)
            .err()
            .unwrap();
        assert!(error.starts_with("desktop_busy"));
        assert!(begin_input("new-grant", Some(901), false, &token).is_ok());
        INPUT.lock().unwrap().as_mut().unwrap().recent.clear();
        assert!(begin_input("new-grant", Some(901), true, &token).is_ok());
        finish_run("new-grant");
        *INPUT.lock().unwrap() = None;
    }
    #[test]
    fn applications_have_a_task_owner_until_release() {
        claim("task-a", "fixture-app").unwrap();
        assert!(claim("task-b", "fixture-app").is_err());
        claim("task-a", "fixture-app").unwrap();
        release("task-a");
        claim("task-b", "fixture-app").unwrap();
        release("task-b");
    }
    #[tokio::test]
    async fn competing_session_can_cancel_without_stealing_desktop() {
        let _serial = SERIAL.lock().unwrap();
        let a = CancellationToken::new();
        let owner = acquire(&a).await.unwrap();
        let b = CancellationToken::new();
        b.cancel();
        assert!(acquire(&b).await.is_err());
        assert!(desktop().try_lock().is_err());
        drop(owner);
        assert!(acquire(&a).await.is_ok());
    }
}
