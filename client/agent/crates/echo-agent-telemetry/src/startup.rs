//! Named startup phases on a per-process timer, reported once to
//! `unified.jsonl`, product events, and OTLP metrics. A closed schema with
//! pinned metric keys: time anything else with a `tracing` span, or give it
//! its own schema.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// `unified.jsonl` message keys, exported so consumers (the probe, tests)
/// grep for the same strings this module writes.
pub const STARTUP_PHASE_MSG: &str = "startup phase";
pub const CONNECT_FINISHED_MSG: &str = "connect finished";
pub const STARTUP_COMPLETE_MSG: &str = "startup complete";
pub const STARTUP_TIMING_MSG: &str = "startup timing";
pub const STARTUP_SLOW_PHASE_MSG: &str = "startup phase running long";

const SLOW_PHASE_WARN_AFTER: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum StartupPhase {
    ConfigLoad,
    ManagedPolicy,
    Bootstrap,
    ModelCatalog,
    WorkerSpawn,
    LeaderConnect,
    AcpInitialize,
    EagerAuth,
    AppInit,
    SessionCreate,
}

impl StartupPhase {
    pub fn label(self) -> &'static str {
        self.into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::IntoStaticStr, serde::Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StartupOutcome {
    Ok,
    Timeout,
    Cancelled,
    Error,
}

impl StartupOutcome {
    pub fn label(self) -> &'static str {
        self.into()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, strum::IntoStaticStr, serde::Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    Unknown,
    Personal,
    Team,
    Deployment,
}

impl AuthMode {
    pub fn label(self) -> &'static str {
        self.into()
    }
}

/// Who reports the timer, so an embedded run does not report it twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Client,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::IntoStaticStr, serde::Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Embedded,
    Leader,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        self.into()
    }
}

#[derive(Clone, Debug)]
pub struct PhaseSnapshot {
    pub completed: Vec<(StartupPhase, Duration)>,
    pub open: Option<(StartupPhase, Duration)>,
}

impl PhaseSnapshot {
    pub fn stuck_in(&self) -> &'static str {
        self.open.map_or("unknown", |(phase, _)| phase.label())
    }

    /// Not the open step: a step with no await inside closes before a deadline
    /// observer can run, so the open one is usually its successor.
    pub fn longest_step(&self) -> Option<StartupPhase> {
        self.completed
            .iter()
            .copied()
            .chain(self.open)
            .max_by_key(|(_, elapsed)| *elapsed)
            .map(|(phase, _)| phase)
    }

    /// Completed phases read `phase=dur`; the open one reads `phase>=dur`.
    pub fn summary(&self) -> String {
        if self.completed.is_empty() && self.open.is_none() {
            return "no phases entered".to_string();
        }
        let mut out = String::new();
        for (phase, d) in &self.completed {
            if !out.is_empty() {
                out.push_str(", ");
            }
            let _ = write!(out, "{}={}", phase.label(), format_duration(*d));
        }
        if let Some((phase, open)) = self.open {
            if !out.is_empty() {
                out.push_str(", ");
            }
            let _ = write!(out, "{}>={}", phase.label(), format_duration(open));
        }
        out
    }
}

struct Inner {
    completed: Vec<(StartupPhase, Duration)>,
    current: Option<(StartupPhase, Instant)>,
    auth_mode: AuthMode,
    owner: Owner,
}

pub struct StartupTimer {
    started: Instant,
    inner: Mutex<Inner>,
}

impl StartupTimer {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            inner: Mutex::new(Inner {
                completed: Vec::new(),
                current: None,
                auth_mode: AuthMode::Unknown,
                owner: Owner::Agent,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Closes the open phase; re-entering the open phase is ignored, so two
    /// layers can name the same step and it is measured once.
    pub fn enter(&self, phase: StartupPhase) {
        let now = Instant::now();
        {
            let mut g = self.lock();
            if matches!(g.current, Some((open, _)) if open == phase) {
                return;
            }
            if let Some((prev, t0)) = g.current.take() {
                g.completed.push((prev, now.saturating_duration_since(t0)));
            }
            g.current = Some((phase, now));
        }
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        tracing::info!(phase = %phase.label(), elapsed_ms, "startup phase");
        crate::unified_log::info(
            STARTUP_PHASE_MSG,
            None,
            Some(serde_json::json!({ "phase": phase.label(), "elapsed_ms": elapsed_ms })),
        );
    }

    fn close_open_phase(&self) {
        let now = Instant::now();
        let mut g = self.lock();
        if let Some((prev, t0)) = g.current.take() {
            g.completed.push((prev, now.saturating_duration_since(t0)));
        }
    }

    pub fn set_auth_mode(&self, mode: AuthMode) {
        self.lock().auth_mode = mode;
    }

    pub fn auth_mode(&self) -> AuthMode {
        self.lock().auth_mode
    }

    pub fn owner(&self) -> Owner {
        self.lock().owner
    }

    /// The open phase and how long it has been open.
    fn open_phase_age(&self) -> Option<(StartupPhase, Duration)> {
        self.lock().current.map(|(p, t0)| (p, t0.elapsed()))
    }

    /// One read, so a caller reporting several facts can't mix moments.
    pub fn phase_snapshot(&self) -> PhaseSnapshot {
        let now = Instant::now();
        let g = self.lock();
        PhaseSnapshot {
            completed: g.completed.clone(),
            open: g
                .current
                .map(|(phase, t0)| (phase, now.saturating_duration_since(t0))),
        }
    }

    pub fn summary(&self) -> String {
        self.phase_snapshot().summary()
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn phase_durations_ms(&self) -> BTreeMap<String, u64> {
        let now = Instant::now();
        let g = self.lock();
        let mut map: BTreeMap<String, u64> = BTreeMap::new();
        for (phase, d) in &g.completed {
            *map.entry(phase.label().to_string()).or_default() += d.as_millis() as u64;
        }
        if let Some((phase, t0)) = g.current {
            *map.entry(phase.label().to_string()).or_default() +=
                now.saturating_duration_since(t0).as_millis() as u64;
        }
        map
    }

    pub fn emit_telemetry(
        &self,
        connect_target: AgentKind,
        outcome: StartupOutcome,
        timeout_secs: Option<u64>,
        embedded_fallback: bool,
    ) {
        // A finished attempt has no open phase; later work is not connect time.
        if outcome == StartupOutcome::Ok {
            self.close_open_phase();
        }
        let timings = self.phase_snapshot();
        let stuck_in = (outcome == StartupOutcome::Timeout).then(|| timings.stuck_in().to_string());
        let phases = timings.summary();
        let elapsed_ms = self.elapsed().as_millis() as u64;
        crate::unified_log::info(
            CONNECT_FINISHED_MSG,
            None,
            Some(serde_json::json!({
                "connect_target": connect_target,
                "outcome": outcome,
                "stuck_in": stuck_in,
                "phases": phases,
                "elapsed_ms": elapsed_ms,
                "auth_mode": self.auth_mode(),
            })),
        );
        crate::session_ctx::log_event(crate::events::AgentConnect {
            connect_target,
            outcome,
            stuck_in,
            phases,
            phase_durations_ms: self.phase_durations_ms(),
            elapsed_ms,
            timeout_secs,
            embedded_fallback,
            auth_mode: self.auth_mode(),
        });
    }
}

impl Default for StartupTimer {
    fn default() -> Self {
        Self::new()
    }
}

static CURRENT: Mutex<Option<Arc<StartupTimer>>> = Mutex::new(None);
static DONE: AtomicBool = AtomicBool::new(false);
static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// A session startup sub-phase routed to its own `*_ms` field, so a producer
/// timer's field is chosen at compile time rather than by string match.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Subphase {
    SessionLoad,
    SessionReplay,
    SessionGitScan,
    SessionSpawn,
}

#[derive(Clone, Copy, Default)]
struct SubphaseTimings {
    prefetch_wait_ms: Option<u64>,
    session_load_ms: Option<u64>,
    session_replay_ms: Option<u64>,
    session_git_scan_ms: Option<u64>,
    session_spawn_ms: Option<u64>,
    time_to_first_frame_ms: Option<u64>,
}

static SUBPHASES: Mutex<SubphaseTimings> = Mutex::new(SubphaseTimings {
    prefetch_wait_ms: None,
    session_load_ms: None,
    session_replay_ms: None,
    session_git_scan_ms: None,
    session_spawn_ms: None,
    time_to_first_frame_ms: None,
});

fn subphases() -> std::sync::MutexGuard<'static, SubphaseTimings> {
    SUBPHASES.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn record_prefetch_wait(elapsed: Duration) {
    subphases().prefetch_wait_ms = Some(elapsed.as_millis() as u64);
}

pub fn record_first_frame() {
    if DONE.load(Ordering::Relaxed) {
        return;
    }
    let elapsed_ms = process_elapsed().as_millis() as u64;
    let mut sub = subphases();
    if sub.time_to_first_frame_ms.is_none() {
        sub.time_to_first_frame_ms = Some(elapsed_ms);
    }
}

pub(crate) fn record_subphase(sp: Subphase, elapsed: Duration) {
    let ms = elapsed.as_millis() as u64;
    let mut sub = subphases();
    match sp {
        Subphase::SessionLoad => sub.session_load_ms = Some(ms),
        Subphase::SessionReplay => sub.session_replay_ms = Some(ms),
        Subphase::SessionGitScan => sub.session_git_scan_ms = Some(ms),
        Subphase::SessionSpawn => sub.session_spawn_ms = Some(ms),
    }
}

/// Call first in `main`; the clock otherwise starts at first use and
/// totals undercount.
pub fn mark_process_start() {
    LazyLock::force(&PROCESS_START);
}

pub fn process_elapsed() -> Duration {
    PROCESS_START.elapsed()
}

fn current() -> Option<Arc<StartupTimer>> {
    CURRENT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(Arc::clone)
}

/// Installs a new attempt, unless startup already ended; after that the
/// returned timer records locally only.
pub fn begin(owner: Owner) -> Arc<StartupTimer> {
    let timer = Arc::new(StartupTimer::new());
    timer.lock().owner = owner;
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if !DONE.load(Ordering::Relaxed) {
        *current = Some(Arc::clone(&timer));
        spawn_slow_phase_warnings();
    }
    timer
}

/// Phases already warned about, per timer.
#[derive(Default)]
struct WarnedPhases {
    timer: usize,
    phases: Vec<StartupPhase>,
}

/// A phase left open past the threshold; agent-owned timers idle with a
/// phase open until their first client, so they are skipped.
fn slow_phase_to_warn(
    timer: &Arc<StartupTimer>,
    threshold: Duration,
    warned: &mut WarnedPhases,
) -> Option<(StartupPhase, Duration)> {
    if timer.owner() == Owner::Agent {
        return None;
    }
    let timer_id = Arc::as_ptr(timer) as usize;
    if warned.timer != timer_id {
        *warned = WarnedPhases {
            timer: timer_id,
            phases: Vec::new(),
        };
    }
    let (phase, age) = timer.open_phase_age()?;
    if age < threshold || warned.phases.contains(&phase) {
        return None;
    }
    warned.phases.push(phase);
    Some((phase, age))
}

/// Warns once per phase that runs long. A plain thread, because startup
/// spans runtime construction; exits when startup ends.
fn spawn_slow_phase_warnings() {
    static SPAWNED: std::sync::Once = std::sync::Once::new();
    SPAWNED.call_once(|| {
        std::thread::Builder::new()
            .name("startup-slow-phase".into())
            .spawn(|| {
                let mut warned = WarnedPhases::default();
                while !DONE.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    let Some(timer) = current() else { continue };
                    if let Some((phase, age)) =
                        slow_phase_to_warn(&timer, SLOW_PHASE_WARN_AFTER, &mut warned)
                    {
                        let open_ms = age.as_millis() as u64;
                        tracing::warn!(
                            phase = phase.label(),
                            open_ms,
                            "startup phase running long"
                        );
                        crate::unified_log::warn(
                            STARTUP_SLOW_PHASE_MSG,
                            None,
                            Some(serde_json::json!({
                                "phase": phase.label(),
                                "open_ms": open_ms,
                            })),
                        );
                    }
                }
            })
            .ok();
    });
}

pub(crate) fn agent_owned() -> Option<Arc<StartupTimer>> {
    current().filter(|p| p.owner() == Owner::Agent)
}

pub(crate) fn is_active() -> bool {
    !DONE.load(Ordering::Relaxed) && current().is_some()
}

fn clear() {
    DONE.store(true, Ordering::Relaxed);
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Stops recording for a standalone agent at its first client, so idle
/// waiting is not counted; client-owned runs are unaffected.
pub fn mark_agent_serving() {
    if agent_owned().is_some() {
        clear();
    }
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    DONE.store(false, Ordering::Relaxed);
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *subphases() = SubphaseTimings::default();
}

/// Lazily installs an agent-owned timer, covering the standalone leader
/// and agent server; a no-op once startup is done.
pub fn enter(phase: StartupPhase) {
    if DONE.load(Ordering::Relaxed) {
        return;
    }
    let timer = match current() {
        Some(timer) => timer,
        None => begin(Owner::Agent),
    };
    timer.enter(phase);
}

/// Scopes a phase to a region of work: entered on creation, closed on drop,
/// so no failure return can leave the phase open across a retry wait.
#[must_use = "the phase closes when this guard drops"]
pub struct PhaseScope(());

impl Drop for PhaseScope {
    fn drop(&mut self) {
        if let Some(timer) = current() {
            timer.close_open_phase();
        }
    }
}

/// Enter `phase` for the lifetime of the returned guard.
pub fn phase_scope(phase: StartupPhase) -> PhaseScope {
    enter(phase);
    PhaseScope(())
}

pub fn set_auth_mode(mode: AuthMode) {
    if let Some(timer) = current() {
        timer.set_auth_mode(mode);
    }
}

/// The obligation to end startup exactly once; a dropped token ends startup
/// itself and logs a warning, so forgotten paths are visible.
#[must_use = "startup must be finished or abandoned"]
pub struct PendingStartup {
    ended: bool,
}

impl PendingStartup {
    /// One per interactive or headless process; utility commands call
    /// [`mark_utility_process`] instead.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        PendingStartup { ended: false }
    }

    /// Records the startup total with `outcome` and ends recording.
    pub fn finish(mut self, outcome: StartupOutcome) {
        report_total(outcome);
        self.ended = true;
    }

    /// Ends recording without a total, for a run the user cancelled or one
    /// that never was a startup.
    pub fn abandon(mut self) {
        clear();
        self.ended = true;
    }

    /// Finishes a token still held in an `Option`; does nothing once taken.
    pub fn finish_held(token: &mut Option<Self>, outcome: StartupOutcome) {
        if let Some(pending) = token.take() {
            pending.finish(outcome);
        }
    }
}

impl Drop for PendingStartup {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        tracing::warn!("startup was never finished; ending recording");
        crate::unified_log::warn("startup never finished", None, None);
        clear();
    }
}

/// Excludes a utility command from startup recording entirely.
pub fn mark_utility_process() {
    clear();
}

/// Records the startup total, at most once per process. A failure the user
/// can retry records nothing, so the eventual success still counts.
pub(crate) fn report_total(outcome: StartupOutcome) {
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let timer = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let total_ms = process_elapsed().as_millis() as u64;
    let (phases, auth_mode) = match timer {
        Some(p) => {
            if outcome == StartupOutcome::Ok {
                p.close_open_phase();
            }
            (p.summary(), p.auth_mode())
        }
        None => (String::new(), AuthMode::Unknown),
    };
    let sub = *subphases();
    let event = crate::events::StartupCompleted {
        total_ms,
        outcome,
        phases,
        auth_mode,
        prefetch_wait_ms: sub.prefetch_wait_ms,
        session_load_ms: sub.session_load_ms,
        session_replay_ms: sub.session_replay_ms,
        session_git_scan_ms: sub.session_git_scan_ms,
        session_spawn_ms: sub.session_spawn_ms,
        time_to_first_frame_ms: sub.time_to_first_frame_ms,
    };
    if let Ok(record) = serde_json::to_value(&event) {
        crate::unified_log::info(STARTUP_COMPLETE_MSG, None, Some(record));
    }
    crate::session_ctx::log_event(event);
}

/// A deadline for a readiness-path network step. Naming the phase and
/// bounding the wait are one call, so neither can be forgotten.
pub struct ReadinessBudget {
    limit: Duration,
}

impl ReadinessBudget {
    pub const fn new(limit: Duration) -> Self {
        Self { limit }
    }

    /// Run `fut` under the budget, attributed to `phase` for exactly the
    /// run's duration. Returns `None` on timeout, after logging, instead of
    /// blocking readiness.
    pub async fn run<T>(
        &self,
        phase: StartupPhase,
        fut: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        let _scope = phase_scope(phase);
        match tokio::time::timeout(self.limit, fut).await {
            Ok(value) => Some(value),
            Err(_) => {
                tracing::warn!(
                    phase = phase.label(),
                    limit_secs = self.limit.as_secs(),
                    "readiness step hit its budget"
                );
                crate::unified_log::warn(
                    "readiness step hit its budget",
                    None,
                    Some(
                        serde_json::json!({ "phase": phase.label(), "limit_secs": self.limit.as_secs() }),
                    ),
                );
                None
            }
        }
    }
}

pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serializes the tests that drive the process-wide startup statics
    // (`CURRENT`/`DONE`/`SUBPHASES` and the redirected unified log); run in
    // parallel they race. Each holder also calls `reset_for_tests` first.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn slow_phase_warning_fires_once_per_open_phase() {
        // `StartupTimer::new` defaults to the exempt `Owner::Agent`.
        let client_timer = || {
            let timer = Arc::new(StartupTimer::new());
            timer.lock().owner = Owner::Client;
            timer
        };
        let timer = client_timer();
        let mut warned = WarnedPhases::default();

        assert!(
            slow_phase_to_warn(&timer, Duration::ZERO, &mut warned).is_none(),
            "no open phase, nothing to warn about",
        );

        timer.enter(StartupPhase::Bootstrap);
        assert!(
            slow_phase_to_warn(&timer, Duration::from_secs(3600), &mut warned).is_none(),
            "a phase within budget stays quiet",
        );
        assert!(matches!(
            slow_phase_to_warn(&timer, Duration::ZERO, &mut warned),
            Some((StartupPhase::Bootstrap, _))
        ));
        assert!(
            slow_phase_to_warn(&timer, Duration::ZERO, &mut warned).is_none(),
            "one warning per phase",
        );

        timer.enter(StartupPhase::SessionCreate);
        assert!(matches!(
            slow_phase_to_warn(&timer, Duration::ZERO, &mut warned),
            Some((StartupPhase::SessionCreate, _))
        ));

        let replacement = client_timer();
        replacement.enter(StartupPhase::Bootstrap);
        assert!(
            slow_phase_to_warn(&replacement, Duration::ZERO, &mut warned).is_some(),
            "a replacement timer warns afresh for the same phase",
        );

        let agent = Arc::new(StartupTimer::new());
        agent.lock().owner = Owner::Agent;
        agent.enter(StartupPhase::Bootstrap);
        assert!(
            slow_phase_to_warn(&agent, Duration::ZERO, &mut warned).is_none(),
            "agent-owned timers idle with a phase open by design",
        );
    }

    #[test]
    fn summary_tracks_completed_and_open_phases() {
        let p = StartupTimer::new();
        p.enter(StartupPhase::ConfigLoad);
        p.enter(StartupPhase::ManagedPolicy);
        p.enter(StartupPhase::ModelCatalog);

        let s = p.summary();
        assert!(s.contains("config_load="), "{s}");
        assert!(s.contains("managed_policy="), "{s}");
        assert!(s.contains("model_catalog>="), "{s}");
        assert_eq!(p.phase_snapshot().stuck_in(), "model_catalog");
        let d = p.phase_durations_ms();
        assert!(
            d.contains_key("config_load") && d.contains_key("model_catalog"),
            "{d:?}"
        );
    }

    // Process-wide statics: `SERIAL` serializes this with the other global
    // tests; interleaved runs race.
    #[test]
    fn global_lifecycle_records_then_ends() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_tests();
        crate::unified_log::redirect_to_temp_for_tests();

        let p = begin(Owner::Client);
        enter(StartupPhase::ManagedPolicy);
        set_auth_mode(AuthMode::Deployment);
        assert_eq!(p.phase_snapshot().stuck_in(), "managed_policy");
        assert_eq!(p.auth_mode().label(), "deployment");
        assert!(
            agent_owned().is_none(),
            "client-owned: agent must not report"
        );

        let p2 = begin(Owner::Client);
        enter(StartupPhase::Bootstrap);
        assert_eq!(p2.phase_snapshot().stuck_in(), "bootstrap");
        assert_eq!(p.phase_snapshot().stuck_in(), "managed_policy");

        drop(crate::instrumentation::timer("startup.mirror_probe_active"));

        let mut git_scan_timer = crate::instrumentation::timer("session.git_divergence");
        git_scan_timer.with_subphase(Subphase::SessionGitScan);
        drop(git_scan_timer);
        record_first_frame();

        enter(StartupPhase::SessionCreate);
        report_total(StartupOutcome::Ok);

        drop(crate::instrumentation::timer("startup.mirror_probe_done"));
        let log = String::from_utf8_lossy(&crate::unified_log::snapshot_log().unwrap_or_default())
            .into_owned();
        assert!(log.contains("startup.mirror_probe_active"), "{log}");
        assert!(
            !log.contains("startup.mirror_probe_done"),
            "done: timers must not mirror, {log}"
        );
        assert!(log.contains("\"session_git_scan_ms\":"), "{log}");
        assert!(log.contains("\"time_to_first_frame_ms\":"), "{log}");

        report_total(StartupOutcome::Ok);
        enter(StartupPhase::ModelCatalog);
        assert_eq!(
            p2.phase_snapshot().stuck_in(),
            "unknown",
            "ok total closes the open phase"
        );
        assert!(p2.summary().contains("session_create="), "{}", p2.summary());
        let p3 = begin(Owner::Agent);
        enter(StartupPhase::ConfigLoad);
        assert_eq!(
            p3.phase_snapshot().stuck_in(),
            "unknown",
            "ended: enter records nothing"
        );

        clear();
        assert!(agent_owned().is_none(), "cleared: nothing installed");

        reset_for_tests();
        mark_utility_process();
        enter(StartupPhase::Bootstrap);
        assert!(agent_owned().is_none(), "utility: nothing records");

        reset_for_tests();
        let p4 = begin(Owner::Client);
        let token = PendingStartup::new();
        enter(StartupPhase::ConfigLoad);
        assert_eq!(p4.phase_snapshot().stuck_in(), "config_load");
        drop(token);
        enter(StartupPhase::Bootstrap);
        assert_eq!(
            p4.phase_snapshot().stuck_in(),
            "config_load",
            "dropped token ended startup"
        );

        record_first_frame();
        let sub = *subphases();
        assert!(
            sub.time_to_first_frame_ms.is_none(),
            "ended startup: draw stamp records nothing"
        );
    }

    // absent≠zero: with no prefetch the caller never stamps, so the record
    // omits `prefetch_wait_ms` rather than reporting a spurious zero.
    #[test]
    fn startup_completed_omits_prefetch_wait_without_a_prefetch() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_tests();
        crate::unified_log::redirect_to_temp_for_tests();

        let _p = begin(Owner::Client);
        enter(StartupPhase::ConfigLoad);
        report_total(StartupOutcome::Ok);

        let log = String::from_utf8_lossy(&crate::unified_log::snapshot_log().unwrap_or_default())
            .into_owned();
        assert!(!log.contains("prefetch_wait_ms"), "{log}");
    }

    #[test]
    fn record_subphase_routes_each_arm_and_first_frame_first_write_wins() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

        let cases = [
            (Subphase::SessionLoad, "session_load"),
            (Subphase::SessionReplay, "session_replay"),
            (Subphase::SessionGitScan, "session_git_scan"),
            (Subphase::SessionSpawn, "session_spawn"),
        ];
        for (sp, name) in cases {
            reset_for_tests();
            record_subphase(sp, Duration::from_millis(7));
            let sub = *subphases();
            let routed = match sp {
                Subphase::SessionLoad => sub.session_load_ms,
                Subphase::SessionReplay => sub.session_replay_ms,
                Subphase::SessionGitScan => sub.session_git_scan_ms,
                Subphase::SessionSpawn => sub.session_spawn_ms,
            };
            assert_eq!(routed, Some(7), "{name} routes to its own field");
            let set = [
                sub.session_load_ms,
                sub.session_replay_ms,
                sub.session_git_scan_ms,
                sub.session_spawn_ms,
            ]
            .iter()
            .filter(|v| v.is_some())
            .count();
            assert_eq!(set, 1, "{name} sets exactly one field");
        }

        reset_for_tests();
        record_first_frame();
        let first = subphases().time_to_first_frame_ms;
        assert!(first.is_some(), "first frame stamps time_to_first_frame_ms");
        subphases().time_to_first_frame_ms = Some(1);
        record_first_frame();
        assert_eq!(
            subphases().time_to_first_frame_ms,
            Some(1),
            "first write wins"
        );
    }
}
