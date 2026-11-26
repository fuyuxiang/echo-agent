use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use echo_agent_foreign_sessions::{
    EnabledForeignSessionSources, ForeignSessionSummary, ForeignSessionTool, RecentForeignSession,
};
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use super::actions::Effect;
use super::app_view::{ActiveView, AppView, SessionPickerEntry};

pub(crate) const RESUME_HINT_WINDOW: std::time::Duration = std::time::Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
pub(crate) struct ForeignResumeLaunch {
    token: u64,
    requested_cwd: PathBuf,
    canonical_cwd: Option<PathBuf>,
    hint: Option<RecentForeignSession>,
}

impl AppView {
    fn has_foreign_resume_startup_conflict(&self) -> bool {
        !self.deferred_startup.is_empty()
    }

    fn foreign_resume_launch_welcome(&self) -> bool {
        self.active_view == ActiveView::Welcome
            && self.auth_return_view.is_none()
            && self.agents.is_empty()
            && self.next_agent_id == 0
            && !self.chat_mode
            && !self.is_zdr_blocked()
            && self.pending_update_version.is_none()
    }

    fn pristine_foreign_resume_welcome(&self) -> bool {
        self.foreign_resume_launch_welcome() && !self.has_foreign_resume_startup_conflict()
    }

    fn foreign_resume_context_matches(&self, launch: &ForeignResumeLaunch) -> bool {
        self.foreign_resume_launch_welcome() && self.cwd == launch.requested_cwd
    }

    fn invalidate_foreign_resume_launch(&mut self) {
        self.foreign_resume_launch_generation =
            self.foreign_resume_launch_generation.wrapping_add(1);
        self.foreign_resume_launch = None;
    }

    pub(crate) fn begin_foreign_resume_detection(&mut self) -> Option<Effect> {
        let compat = self.foreign_session_compat;
        if self.foreign_resume_launch.is_some()
            || !(compat.claude || compat.codex || compat.cursor)
            || !self.pristine_foreign_resume_welcome()
        {
            return None;
        }
        self.foreign_resume_launch_generation =
            self.foreign_resume_launch_generation.wrapping_add(1);
        let token = self.foreign_resume_launch_generation;
        let requested_cwd = self.cwd.clone();
        self.foreign_resume_launch = Some(ForeignResumeLaunch {
            token,
            requested_cwd: requested_cwd.clone(),
            canonical_cwd: None,
            hint: None,
        });
        Some(Effect::CanonicalizeForeignResumeCwd {
            requested_cwd,
            launch_token: token,
        })
    }

    pub(crate) fn accept_foreign_resume_canonical_cwd(
        &mut self,
        token: u64,
        requested_cwd: &Path,
        canonical_cwd: Option<PathBuf>,
    ) -> bool {
        let matches_launch = self
            .foreign_resume_launch
            .as_ref()
            .is_some_and(|launch| launch.token == token && launch.requested_cwd == requested_cwd);
        if !matches_launch {
            return false;
        }
        let Some(canonical_cwd) = canonical_cwd.filter(|_| {
            self.pristine_foreign_resume_welcome()
                && self
                    .foreign_resume_launch
                    .as_ref()
                    .is_some_and(|launch| self.foreign_resume_context_matches(launch))
        }) else {
            self.invalidate_foreign_resume_launch();
            return false;
        };
        if let Some(launch) = self.foreign_resume_launch.as_mut() {
            launch.canonical_cwd = Some(canonical_cwd);
            true
        } else {
            false
        }
    }

    pub(crate) fn apply_foreign_resume_detection(
        &mut self,
        token: u64,
        canonical_cwd: &Path,
        hint: Option<RecentForeignSession>,
    ) {
        let matches_launch = self.foreign_resume_launch.as_ref().is_some_and(|launch| {
            launch.token == token && launch.canonical_cwd.as_deref() == Some(canonical_cwd)
        });
        if !matches_launch {
            return;
        }
        let Some(hint) = hint.filter(|_| {
            self.pristine_foreign_resume_welcome()
                && self
                    .foreign_resume_launch
                    .as_ref()
                    .is_some_and(|launch| self.foreign_resume_context_matches(launch))
        }) else {
            self.invalidate_foreign_resume_launch();
            return;
        };
        if let Some(launch) = self.foreign_resume_launch.as_mut() {
            launch.hint = Some(hint);
        }
    }

    pub(crate) fn foreign_resume_hint(&self) -> Option<&RecentForeignSession> {
        self.foreign_resume_launch
            .as_ref()
            .filter(|launch| self.foreign_resume_context_matches(launch))
            .and_then(|launch| launch.hint.as_ref())
    }

    pub(crate) fn take_foreign_resume_hint(&mut self) -> Option<RecentForeignSession> {
        let hint = self
            .foreign_resume_launch
            .as_ref()
            .filter(|launch| self.foreign_resume_context_matches(launch))
            .and_then(|launch| launch.hint.clone())?;
        self.invalidate_foreign_resume_launch();
        Some(hint)
    }

    pub(crate) fn reconcile_foreign_resume_launch(&mut self) {
        let invalid = self.foreign_resume_launch.as_ref().is_some_and(|launch| {
            !self.foreign_resume_context_matches(launch)
                || (launch.hint.is_none() && self.has_foreign_resume_startup_conflict())
        });
        if invalid {
            self.invalidate_foreign_resume_launch();
        }
    }
}

/// Opaque per-application coordinator for foreign session scans.
#[derive(Debug, Clone)]
pub struct ForeignScanCoordinator {
    inner: Arc<ForeignScanCoordinatorInner>,
}

#[derive(Debug)]
struct ForeignScanCoordinatorInner {
    latest_seq: Arc<AtomicU64>,
    semaphore: Arc<Semaphore>,
    abort_handle: Mutex<Option<tokio::task::AbortHandle>>,
}

impl Drop for ForeignScanCoordinatorInner {
    fn drop(&mut self) {
        // An already-running spawn_blocking closure remains non-cancellable.
        if let Some(handle) = self.abort_handle.get_mut().take() {
            handle.abort();
        }
    }
}

impl Default for ForeignScanCoordinator {
    fn default() -> Self {
        Self {
            inner: Arc::new(ForeignScanCoordinatorInner {
                latest_seq: Arc::new(AtomicU64::new(0)),
                semaphore: Arc::new(Semaphore::new(1)),
                abort_handle: Mutex::new(None),
            }),
        }
    }
}

impl ForeignScanCoordinator {
    pub(crate) fn begin_request(&self, seq: u64) {
        self.inner.latest_seq.store(seq, Ordering::Release);
        if let Some(handle) = self.inner.abort_handle.lock().take() {
            handle.abort();
        }
    }

    pub(crate) fn install_abort_handle(&self, seq: u64, handle: tokio::task::AbortHandle) {
        if self.latest_seq() != seq {
            handle.abort();
            return;
        }
        if let Some(previous) = self.inner.abort_handle.lock().replace(handle) {
            previous.abort();
        }
    }

    pub(crate) fn latest_seq(&self) -> u64 {
        self.inner.latest_seq.load(Ordering::Acquire)
    }

    pub(crate) fn latest_seq_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.inner.latest_seq)
    }

    pub(crate) fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.inner.semaphore)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ForeignPickerSource {
    Claude,
    Codex,
    Cursor,
}

impl ForeignPickerSource {
    const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Cursor];

    pub(crate) fn from_tool(tool: ForeignSessionTool) -> Self {
        match tool {
            ForeignSessionTool::Claude => Self::Claude,
            ForeignSessionTool::Codex => Self::Codex,
            ForeignSessionTool::Cursor => Self::Cursor,
        }
    }

    pub(crate) const fn tool(self) -> ForeignSessionTool {
        match self {
            Self::Claude => ForeignSessionTool::Claude,
            Self::Codex => ForeignSessionTool::Codex,
            Self::Cursor => ForeignSessionTool::Cursor,
        }
    }

    pub(crate) fn from_picker_source(source: &str) -> Option<Self> {
        match source {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" => Some(Self::Cursor),
            _ => None,
        }
    }

    pub(crate) const fn picker_source(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
        }
    }

    pub(crate) const fn display_label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
        }
    }

    const fn skill_name(self) -> &'static str {
        match self {
            Self::Claude => "resume-claude",
            Self::Codex => "resume-codex",
            Self::Cursor => "resume-cursor",
        }
    }

    fn compat_enabled(self, compat: EnabledForeignSessionSources) -> bool {
        match self {
            Self::Claude => compat.claude,
            Self::Codex => compat.codex,
            Self::Cursor => compat.cursor,
        }
    }

    fn set_enabled(self, enabled: &mut EnabledForeignSessionSources) {
        match self {
            Self::Claude => enabled.claude = true,
            Self::Codex => enabled.codex = true,
            Self::Cursor => enabled.cursor = true,
        }
    }

    pub(crate) fn resume_prompt(self, native_id: &str) -> String {
        format!("/{} {native_id}", self.skill_name())
    }

    fn skill_paths(self, echo_agent_home: &Path) -> [PathBuf; 2] {
        let skill = self.skill_name();
        [
            echo_agent_home
                .join("bundled")
                .join("skills")
                .join(skill)
                .join("SKILL.md"),
            echo_agent_home.join("skills").join(skill).join("SKILL.md"),
        ]
    }
}

pub(crate) fn is_foreign_picker_source(source: &str) -> bool {
    ForeignPickerSource::from_picker_source(source).is_some()
}

pub(crate) fn badge_for_picker_source(source: &str) -> &'static str {
    if source == "conversation" {
        "chat"
    } else {
        ForeignPickerSource::from_picker_source(source)
            .map(ForeignPickerSource::picker_source)
            .unwrap_or("")
    }
}

pub(crate) fn foreign_tool_display_label(tool: ForeignSessionTool) -> &'static str {
    ForeignPickerSource::from_tool(tool).display_label()
}

pub(crate) async fn gated_sources_async_with<F, Fut>(
    compat: EnabledForeignSessionSources,
    echo_agent_home: &Path,
    mut metadata_exists: F,
) -> EnabledForeignSessionSources
where
    F: FnMut(PathBuf) -> Fut,
    Fut: Future<Output = bool>,
{
    let mut enabled = EnabledForeignSessionSources::default();
    for source in ForeignPickerSource::ALL {
        if !source.compat_enabled(compat) {
            continue;
        }
        let mut available = false;
        for path in source.skill_paths(echo_agent_home) {
            if metadata_exists(path).await {
                available = true;
                break;
            }
        }
        if available {
            source.set_enabled(&mut enabled);
        }
    }
    enabled
}

pub(crate) async fn gated_sources_async(
    compat: EnabledForeignSessionSources,
    echo_agent_home: &Path,
) -> EnabledForeignSessionSources {
    gated_sources_async_with(compat, echo_agent_home, |path| async move {
        tokio::fs::metadata(path).await.is_ok()
    })
    .await
}

pub(crate) async fn with_gated_sources_async_with<F, Fut, W, WorkFut, T>(
    compat: EnabledForeignSessionSources,
    echo_agent_home: &Path,
    metadata_exists: F,
    work: W,
) -> Option<T>
where
    F: FnMut(PathBuf) -> Fut,
    Fut: Future<Output = bool>,
    W: FnOnce(EnabledForeignSessionSources) -> WorkFut,
    WorkFut: Future<Output = T>,
{
    let enabled = gated_sources_async_with(compat, echo_agent_home, metadata_exists).await;
    if !(enabled.claude || enabled.codex || enabled.cursor) {
        return None;
    }
    Some(work(enabled).await)
}

pub(crate) async fn with_gated_sources_async<W, WorkFut, T>(
    compat: EnabledForeignSessionSources,
    echo_agent_home: &Path,
    work: W,
) -> Option<T>
where
    W: FnOnce(EnabledForeignSessionSources) -> WorkFut,
    WorkFut: Future<Output = T>,
{
    with_gated_sources_async_with(
        compat,
        echo_agent_home,
        |path| async move { tokio::fs::metadata(path).await.is_ok() },
        work,
    )
    .await
}

pub(crate) fn scan_effect(
    cwd: &Path,
    compat: EnabledForeignSessionSources,
    echo_agent_home: &Path,
    coordinator: ForeignScanCoordinator,
    seq: u64,
) -> Option<Effect> {
    coordinator.begin_request(seq);
    (compat.claude || compat.codex || compat.cursor).then(|| Effect::ScanForeignSessions {
        cwd: cwd.to_path_buf(),
        compat,
        echo_agent_home: echo_agent_home.to_path_buf(),
        coordinator,
        seq,
    })
}

pub(crate) fn map_summary(summary: ForeignSessionSummary) -> SessionPickerEntry {
    let source = ForeignPickerSource::from_tool(summary.tool);
    let updated_at = chrono::DateTime::<chrono::Utc>::from(summary.updated_at);
    let cwd = summary.cwd.to_string_lossy().into_owned();
    SessionPickerEntry {
        id: summary.native_id,
        summary: summary.title,
        updated_at,
        created_at: updated_at,
        cwd: cwd.clone(),
        hostname: None,
        source: source.picker_source().to_owned(),
        model_id: None,
        num_messages: 0,
        last_active_at: Some(updated_at),
        branch: summary.branch,
        repo_name: crate::views::session_picker::repo_name_from_cwd(&cwd),
        worktree_label: None,
        last_turn_summary: None,
        last_recap: None,
        card_detail: None,
    }
}

fn entry_recency(entry: &SessionPickerEntry) -> chrono::DateTime<chrono::Utc> {
    entry.last_active_at.unwrap_or(entry.updated_at)
}

fn sort_picker_entries(entries: &mut [SessionPickerEntry]) {
    entries.sort_by(|left, right| {
        entry_recency(right)
            .cmp(&entry_recency(left))
            .then_with(|| {
                match (
                    ForeignPickerSource::from_picker_source(&left.source),
                    ForeignPickerSource::from_picker_source(&right.source),
                ) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (Some(_), None) => std::cmp::Ordering::Greater,
                    (Some(left_source), Some(right_source)) => left_source
                        .cmp(&right_source)
                        .then_with(|| left.id.cmp(&right.id)),
                }
            })
    });
}

pub(crate) fn replace_foreign_entries(
    entries: &mut Option<Vec<SessionPickerEntry>>,
    mut foreign: Vec<SessionPickerEntry>,
) {
    foreign.retain(|entry| is_foreign_picker_source(&entry.source));
    sort_picker_entries(&mut foreign);
    let mut seen = HashSet::new();
    foreign.retain(|entry| seen.insert((entry.source.clone(), entry.id.clone())));

    let mut merged = entries.take().unwrap_or_default();
    merged.retain(|entry| !is_foreign_picker_source(&entry.source));
    let has_foreign = !foreign.is_empty();
    merged.extend(foreign);
    if has_foreign {
        sort_picker_entries(&mut merged);
    }
    *entries = (!merged.is_empty()).then_some(merged);
}

pub(crate) fn replace_native_entries(
    entries: &mut Option<Vec<SessionPickerEntry>>,
    mut native: Vec<SessionPickerEntry>,
) {
    let foreign: Vec<_> = entries
        .take()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| is_foreign_picker_source(&entry.source))
        .collect();
    if foreign.is_empty() {
        *entries = (!native.is_empty()).then_some(native);
        return;
    }
    native.extend(foreign);
    sort_picker_entries(&mut native);
    *entries = (!native.is_empty()).then_some(native);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::{Duration, UNIX_EPOCH};

    use echo_agent_foreign_sessions::ForeignSessionSource;

    use super::*;

    struct CancellationSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for CancellationSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    fn compat_all() -> EnabledForeignSessionSources {
        EnabledForeignSessionSources {
            claude: true,
            codex: true,
            cursor: true,
        }
    }

    fn picker_entry(id: &str, source: &str, timestamp: i64) -> SessionPickerEntry {
        let timestamp = chrono::DateTime::from_timestamp(timestamp, 0).unwrap();
        SessionPickerEntry {
            id: id.into(),
            summary: id.into(),
            updated_at: timestamp,
            created_at: timestamp,
            cwd: "/repo".into(),
            hostname: None,
            source: source.into(),
            model_id: None,
            num_messages: 0,
            last_active_at: None,
            branch: None,
            repo_name: "repo".into(),
            worktree_label: None,
            last_turn_summary: None,
            last_recap: None,
            card_detail: None,
        }
    }

    #[tokio::test]
    async fn async_gate_checks_compat_before_skill_metadata() {
        let probed = RefCell::new(Vec::new());
        let enabled = gated_sources_async_with(
            EnabledForeignSessionSources::default(),
            Path::new("/echoagent"),
            |path| {
                probed.borrow_mut().push(path.to_path_buf());
                std::future::ready(true)
            },
        )
        .await;
        assert_eq!(enabled, EnabledForeignSessionSources::default());
        assert!(probed.borrow().is_empty());
    }

    #[tokio::test]
    async fn async_gate_missing_skill_prevents_store_work() {
        let probed = RefCell::new(Vec::new());
        let store_calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let store_calls_for_work = std::rc::Rc::clone(&store_calls);
        let result = with_gated_sources_async_with(
            EnabledForeignSessionSources {
                codex: true,
                ..Default::default()
            },
            Path::new("/echoagent"),
            |path| {
                probed.borrow_mut().push(path.to_path_buf());
                std::future::ready(false)
            },
            move |enabled| async move {
                store_calls_for_work.set(store_calls_for_work.get() + 1);
                enabled
            },
        )
        .await;
        assert!(result.is_none());
        let probed = probed.borrow();
        assert_eq!(probed.len(), 2);
        assert!(
            probed
                .iter()
                .all(|path| path.to_string_lossy().contains("resume-codex"))
        );
        assert_eq!(store_calls.get(), 0);
    }

    #[tokio::test]
    async fn async_gate_supports_bundled_and_user_skill_locations() {
        let enabled = gated_sources_async_with(compat_all(), Path::new("/echoagent"), |path| {
            let path = path.to_string_lossy();
            std::future::ready(
                path.contains("bundled/skills/resume-claude")
                    || path.contains("skills/resume-codex")
                    || path.contains("bundled/skills/resume-cursor"),
            )
        })
        .await;
        assert_eq!(
            enabled,
            EnabledForeignSessionSources {
                claude: true,
                codex: true,
                cursor: true,
            }
        );
    }

    #[test]
    fn launch_detection_schedules_once_only_for_pristine_welcome() {
        let mut app = crate::app::app_view::tests::test_app();
        app.foreign_session_compat = EnabledForeignSessionSources {
            cursor: true,
            ..Default::default()
        };
        app.deferred_startup.prompt = Some("explicit startup".into());
        assert!(app.begin_foreign_resume_detection().is_none());
        app.deferred_startup.prompt = None;
        let Some(Effect::CanonicalizeForeignResumeCwd {
            requested_cwd,
            launch_token,
        }) = app.begin_foreign_resume_detection()
        else {
            panic!("expected canonicalization effect");
        };
        assert!(
            app.begin_foreign_resume_detection().is_none(),
            "one launch must schedule at most one detection"
        );
        app.active_view = crate::app::app_view::ActiveView::AgentDashboard;
        app.reconcile_foreign_resume_launch();
        app.active_view = crate::app::app_view::ActiveView::Welcome;
        assert!(!app.accept_foreign_resume_canonical_cwd(
            launch_token,
            &requested_cwd,
            dunce::canonicalize(&requested_cwd).ok(),
        ));
        assert!(app.foreign_resume_hint().is_none());
    }

    #[test]
    fn scan_effect_defers_skill_gate_to_background_lane() {
        let home = tempfile::tempdir().unwrap();
        let coordinator = ForeignScanCoordinator::default();
        let effect = scan_effect(
            Path::new("/repo"),
            compat_all(),
            home.path(),
            coordinator,
            2,
        )
        .expect("compat-enabled sources schedule a background gate");
        assert!(matches!(
            effect,
            Effect::ScanForeignSessions {
                compat: EnabledForeignSessionSources {
                    claude: true,
                    codex: true,
                    cursor: true,
                },
                seq: 2,
                ..
            }
        ));
        assert!(
            scan_effect(
                Path::new("/repo"),
                EnabledForeignSessionSources::default(),
                home.path(),
                ForeignScanCoordinator::default(),
                3,
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn scan_coordinator_aborts_obsolete_pending_task() {
        let coordinator = ForeignScanCoordinator::default();
        coordinator.begin_request(1);
        let first = tokio::spawn(std::future::pending::<()>());
        coordinator.install_abort_handle(1, first.abort_handle());

        coordinator.begin_request(2);

        let join_error = tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .expect("obsolete task cancellation timed out")
            .expect_err("obsolete task must be cancelled");
        assert!(join_error.is_cancelled());
        assert_eq!(coordinator.latest_seq(), 2);
    }

    #[test]
    fn scan_coordinators_are_independent_and_clones_share_state() {
        let first = ForeignScanCoordinator::default();
        let second = ForeignScanCoordinator::default();
        first.begin_request(3);
        assert_eq!(first.latest_seq(), 3);
        assert_eq!(second.latest_seq(), 0);

        let first_clone = first.clone();
        first_clone.begin_request(4);
        assert_eq!(first.latest_seq(), 4);
        assert_eq!(second.latest_seq(), 0);
    }

    #[tokio::test]
    async fn final_coordinator_drop_aborts_pending_outer_task() {
        let coordinator = ForeignScanCoordinator::default();
        coordinator.begin_request(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
        let (probe_tx, mut probe_rx) =
            tokio::sync::mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<()>>();
        let task = tokio::spawn(async move {
            let _signal = CancellationSignal(Some(cancelled_tx));
            let _ = started_tx.send(());
            while let Some(acknowledge) = probe_rx.recv().await {
                let _ = acknowledge.send(());
            }
        });
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .expect("pending task start timed out")
            .expect("pending task started");
        coordinator.install_abort_handle(1, task.abort_handle());

        let clone = coordinator.clone();
        drop(coordinator);
        let (acknowledge_tx, acknowledge_rx) = tokio::sync::oneshot::channel();
        probe_tx
            .send(acknowledge_tx)
            .expect("pending task accepts liveness probe");
        tokio::time::timeout(Duration::from_secs(1), acknowledge_rx)
            .await
            .expect("liveness probe timed out")
            .expect("non-final drop keeps task alive");

        drop(clone);
        tokio::time::timeout(Duration::from_secs(1), cancelled_rx)
            .await
            .expect("cancellation signal timed out")
            .expect("final drop cancels task");
        let join_error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancelled task join timed out")
            .expect_err("final drop must abort the task");
        assert!(join_error.is_cancelled());
    }

    #[test]
    fn source_mapping_owns_badges_and_prompts() {
        for (source, wire, prompt) in [
            (
                ForeignPickerSource::Claude,
                "claude",
                "/resume-claude native-id",
            ),
            (
                ForeignPickerSource::Codex,
                "codex",
                "/resume-codex native-id",
            ),
            (
                ForeignPickerSource::Cursor,
                "cursor",
                "/resume-cursor native-id",
            ),
        ] {
            assert_eq!(source.picker_source(), wire);
            assert_eq!(source.resume_prompt("native-id"), prompt);
            assert_eq!(ForeignPickerSource::from_picker_source(wire), Some(source));
        }
        assert_eq!(badge_for_picker_source("conversation"), "chat");
        assert_eq!(badge_for_picker_source("local"), "");
    }

    #[test]
    fn summary_mapping_collapses_cursor_and_codex_store_variants() {
        for (tool, store_source, picker_source) in [
            (
                ForeignSessionTool::Claude,
                ForeignSessionSource::ClaudeCode,
                "claude",
            ),
            (
                ForeignSessionTool::Codex,
                ForeignSessionSource::CodexCli,
                "codex",
            ),
            (
                ForeignSessionTool::Codex,
                ForeignSessionSource::CodexVsCode,
                "codex",
            ),
            (
                ForeignSessionTool::Codex,
                ForeignSessionSource::CodexAtlas,
                "codex",
            ),
            (
                ForeignSessionTool::Codex,
                ForeignSessionSource::CodexChatGpt,
                "codex",
            ),
            (
                ForeignSessionTool::Cursor,
                ForeignSessionSource::CursorDesktop,
                "cursor",
            ),
            (
                ForeignSessionTool::Cursor,
                ForeignSessionSource::CursorCli,
                "cursor",
            ),
        ] {
            let entry = map_summary(ForeignSessionSummary {
                tool,
                source: store_source,
                native_id: "id".into(),
                title: "title".into(),
                cwd: PathBuf::from("/work/repo"),
                updated_at: UNIX_EPOCH + Duration::from_secs(42),
                branch: Some("main".into()),
            });
            assert_eq!(entry.source, picker_source);
            assert_eq!(entry.id, "id");
            assert_eq!(entry.repo_name, "work-repo");
            assert_eq!(entry.branch.as_deref(), Some("main"));
            assert_eq!(entry.last_active_at, Some(entry.updated_at));
        }
    }

    #[test]
    fn replacing_foreign_rows_preserves_native_and_clears_stale_sources() {
        let native = picker_entry("native", "local", 1);
        let stale = picker_entry("stale", "claude", 1);
        let mut entries = Some(vec![native, stale]);

        replace_foreign_entries(&mut entries, vec![]);

        let entries = entries.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "native");
    }

    #[test]
    fn equal_recency_keeps_native_first_and_orders_foreign_deterministically() {
        let mut entries = Some(vec![picker_entry("native", "local", 1)]);

        replace_foreign_entries(
            &mut entries,
            vec![
                picker_entry("z", "codex", 1),
                picker_entry("b", "claude", 1),
                picker_entry("a", "claude", 1),
            ],
        );

        let ids: Vec<_> = entries.unwrap().into_iter().map(|entry| entry.id).collect();
        assert_eq!(ids, ["native", "a", "b", "z"]);
    }
}
