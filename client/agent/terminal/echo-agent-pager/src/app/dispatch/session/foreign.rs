use crate::app::actions::Effect;
use crate::app::app_view::{AppView, SessionPickerEntry};
use crate::app::dispatch::ctx::get_active_agent_mut;
use crate::app::effects::ConversationsPartial;
use crate::views::modal::ActiveModal;
use crate::views::picker::PickerState;
use crate::views::session_picker::{
    PickerSelectionAnchor, SessionPickerLanes, SessionPickerPendingNotice, SourceFilter,
    capture_picker_selection, effective_filter_query, repo_name_from_cwd, restore_picker_selection,
};

use echo_agent_runtime::session::unified_list::ListScope;

type SearchHit = echo_agent_runtime::extensions::session_search::SearchSessionHit;

struct PickerSurface<'a> {
    entries: &'a mut Option<Vec<SessionPickerEntry>>,
    loading: &'a mut bool,
    lanes: &'a mut SessionPickerLanes,
    state: &'a mut PickerState,
    content_results: &'a mut Option<Vec<SearchHit>>,
    content_loading: &'a mut bool,
    entries_query: &'a mut Option<String>,
    source_filter: SourceFilter,
    grouped: bool,
    current_repo: String,
}

impl PickerSurface<'_> {
    fn capture_selection(&self) -> PickerSelectionAnchor {
        capture_picker_selection(
            self.entries.as_deref(),
            self.content_results.as_deref(),
            self.state,
            effective_filter_query(self.state.query(), self.entries_query.as_deref()),
            self.grouped,
            *self.content_loading,
            self.source_filter,
            Some(&self.current_repo),
        )
    }

    fn restore_selection(&mut self, anchor: PickerSelectionAnchor) {
        let filter_query =
            effective_filter_query(self.state.query(), self.entries_query.as_deref()).to_owned();
        restore_picker_selection(
            anchor,
            self.entries.as_deref(),
            self.content_results.as_deref(),
            self.state,
            &filter_query,
            self.grouped,
            *self.content_loading,
            self.source_filter,
            Some(&self.current_repo),
        );
        self.state.expanded.clear();
    }

    fn native_loaded(
        &mut self,
        sessions: Vec<SessionPickerEntry>,
        query: Option<String>,
        chat_mode: bool,
        empty_notice: String,
        partial_notice: Option<&'static str>,
    ) -> Option<String> {
        let anchor = self.capture_selection();
        let is_search = query.is_some();
        *self.loading = false;
        if is_search {
            *self.content_loading = false;
        }
        *self.entries_query = query;
        if chat_mode {
            *self.entries = (!sessions.is_empty()).then_some(sessions);
        } else {
            crate::app::foreign_sessions::replace_native_entries(self.entries, sessions);
        }
        if is_search && self.entries.is_none() {
            *self.entries = Some(Vec::new());
        }
        let notice = if self.entries.is_none() && !is_search {
            if self.lanes.foreign_loading {
                self.lanes.pending_notice = Some(SessionPickerPendingNotice::Empty(empty_notice));
                None
            } else {
                self.lanes.pending_notice = None;
                Some(empty_notice)
            }
        } else {
            self.lanes.pending_notice = None;
            if chat_mode {
                partial_notice.map(str::to_owned)
            } else {
                None
            }
        };
        self.restore_selection(anchor);
        notice
    }

    fn native_failed(
        &mut self,
        error_notice: String,
        is_search: bool,
        chat_mode: bool,
    ) -> Option<String> {
        let anchor = self.capture_selection();
        *self.loading = false;
        if is_search {
            *self.content_loading = false;
        }
        if chat_mode {
            *self.entries = None;
        } else {
            crate::app::foreign_sessions::replace_native_entries(self.entries, Vec::new());
        }
        *self.entries_query = None;
        let notice = if self.lanes.foreign_loading {
            self.lanes.pending_notice = Some(SessionPickerPendingNotice::Error(error_notice));
            None
        } else {
            self.lanes.pending_notice = None;
            Some(error_notice)
        };
        self.restore_selection(anchor);
        notice
    }

    fn foreign_loaded(&mut self, scanned: Vec<SessionPickerEntry>) -> Option<String> {
        let anchor = self.capture_selection();
        crate::app::foreign_sessions::replace_foreign_entries(self.entries, scanned);
        self.lanes.foreign_loading = false;
        let notice = self.lanes.take_ready_notice(self.entries.is_some());
        self.restore_selection(anchor);
        notice
    }
}

/// Kind facet for welcome multi-source history under `--chat`.
///
/// Sandbox → `chat` (gateway); Local → `build` (local-disk). Modal / non-welcome
/// fetches leave this `None` so the shell keeps its default chat-mode force.
pub(in crate::app::dispatch) fn welcome_history_kind_filter(app: &AppView) -> Option<Vec<String>> {
    #[cfg(feature = "local-workspace")]
    {
        if app.chat_mode && matches!(app.active_view, crate::app::app_view::ActiveView::Welcome) {
            return Some(vec![
                app.welcome_workspace_mode.history_kind_filter().to_string(),
            ]);
        }
    }
    let _ = app;
    None
}

pub(in crate::app::dispatch) fn dispatch_fetch_session_list(app: &mut AppView) -> Vec<Effect> {
    app.session_picker_detail_generation += 1;
    app.session_picker_loading = true;
    app.session_picker_entries = None;
    app.session_picker_state.selected = 0;
    app.session_picker_state.set_query("");
    app.session_picker_state.search_active = false;
    app.session_picker_state.expanded.clear();
    app.session_picker_content_results = None;
    app.session_picker_content_loading = false;
    app.session_picker_entries_query = None;
    if app.chat_mode {
        app.session_picker_list_seq += 1;
    }
    app.foreign_session_scan_seq += 1;
    let foreign_seq = app.foreign_session_scan_seq;
    let kind_filter = welcome_history_kind_filter(app);
    #[cfg(feature = "local-workspace")]
    crate::views::welcome::workspace_mode::log_history_source(
        "session_list_fetch_dispatch",
        Some(app.welcome_workspace_mode),
        kind_filter.as_deref(),
        None,
    );
    let mut effects = vec![Effect::FetchSessionList {
        query: None,
        seq: app.session_picker_list_seq,
        kind_filter,
    }];
    let foreign_effect = if app.chat_mode {
        app.foreign_scan_coordinator.begin_request(foreign_seq);
        None
    } else {
        let echo_agent_home = echo_agent_tools::util::echo_agent_home::echo_agent_home();
        crate::app::foreign_sessions::scan_effect(
            &app.cwd,
            app.foreign_session_compat,
            &echo_agent_home,
            app.foreign_scan_coordinator.clone(),
            foreign_seq,
        )
    };
    let foreign_loading = foreign_effect.is_some();
    let mut modal_lanes_set = false;
    if let Some(agent) = get_active_agent_mut(app)
        && let Some(ActiveModal::SessionPicker { lanes, .. }) = agent.active_modal.as_mut()
    {
        lanes.foreign_loading = foreign_loading;
        lanes.pending_notice = None;
        modal_lanes_set = true;
    }
    app.session_picker_lanes.foreign_loading = foreign_loading && !modal_lanes_set;
    app.session_picker_lanes.pending_notice = None;
    effects.extend(foreign_effect);
    effects
}

pub(in crate::app::dispatch) fn handle_session_list_loaded(
    app: &mut AppView,
    sessions: Vec<SessionPickerEntry>,
    partial: Option<ConversationsPartial>,
    scope: ListScope,
    seq: u64,
    query: Option<String>,
) -> Vec<Effect> {
    if seq != app.session_picker_list_seq {
        return vec![];
    }
    app.session_picker_detail_generation += 1;
    if let Some(partial) = partial {
        crate::unified_log::warn(
            "session.list.partial",
            None,
            Some(serde_json::json!({ "reason": format!("{partial:?}") })),
        );
    }
    let empty_notice = partial.map_or_else(
        || "No sessions found for this directory".to_owned(),
        |partial| partial.picker_notice().to_owned(),
    );
    let partial_notice = partial.map(ConversationsPartial::picker_notice);
    let chat_mode = app.chat_mode;
    let is_browse = query.is_none();
    let mut sessions = Some(sessions);
    let mut notice = None;
    if let Some(agent) = get_active_agent_mut(app) {
        let current_repo = repo_name_from_cwd(&agent.session.cwd.to_string_lossy());
        if let Some(ActiveModal::SessionPicker {
            entries,
            loading,
            lanes,
            state,
            content_results,
            content_loading,
            entries_query,
            source_filter,
            ..
        }) = agent.active_modal.as_mut()
        {
            notice = PickerSurface {
                entries,
                loading,
                lanes,
                state,
                content_results,
                content_loading,
                entries_query,
                source_filter: *source_filter,
                grouped: true,
                current_repo,
            }
            .native_loaded(
                sessions.take().unwrap_or_default(),
                query.clone(),
                chat_mode,
                empty_notice.clone(),
                partial_notice,
            );
        }
    }
    if let Some(sessions) = sessions {
        let current_repo = repo_name_from_cwd(&app.cwd.to_string_lossy());
        notice = PickerSurface {
            entries: &mut app.session_picker_entries,
            loading: &mut app.session_picker_loading,
            lanes: &mut app.session_picker_lanes,
            state: &mut app.session_picker_state,
            content_results: &mut app.session_picker_content_results,
            content_loading: &mut app.session_picker_content_loading,
            entries_query: &mut app.session_picker_entries_query,
            source_filter: app.session_picker_source_filter,
            grouped: app.session_picker_grouped,
            current_repo,
        }
        .native_loaded(sessions, query, chat_mode, empty_notice, partial_notice);
    }
    if let Some(notice) = notice {
        app.show_toast(&notice);
    } else if scope.is_relaxed()
        && app.session_picker_relaxed_notified_for.as_deref() != Some(app.cwd.as_path())
        // Welcome view drops toasts; don't consume the one-shot notice unless
        // it can render.
        && !matches!(app.active_view, crate::app::app_view::ActiveView::Welcome)
    {
        // Notify once per directory; the browse is scoped to `app.cwd`.
        app.session_picker_relaxed_notified_for = Some(app.cwd.clone());
        let message = match scope {
            ListScope::Repo => {
                "No sessions in this directory. Showing other sessions from this repository."
            }
            _ => "No sessions in this directory. Showing sessions from other directories.",
        };
        app.show_toast(message);
    }
    // A cwd-scoped browse clears the latch so a later relax re-notifies; search
    // responses leave it alone.
    if !scope.is_relaxed() && is_browse {
        app.session_picker_relaxed_notified_for = None;
    }
    vec![]
}

pub(in crate::app::dispatch) fn handle_session_list_failed(
    app: &mut AppView,
    error: String,
    seq: u64,
    query: Option<String>,
) -> Vec<Effect> {
    if seq != app.session_picker_list_seq {
        return vec![];
    }
    app.session_picker_detail_generation += 1;
    tracing::warn!(error = %error, "session list fetch failed");
    let error_notice = format!("Couldn't load sessions: {error}");
    let is_search = query.is_some();
    let chat_mode = app.chat_mode;
    let mut handled = false;
    let mut notice = None;
    if let Some(agent) = get_active_agent_mut(app) {
        let current_repo = repo_name_from_cwd(&agent.session.cwd.to_string_lossy());
        if let Some(ActiveModal::SessionPicker {
            entries,
            loading,
            lanes,
            state,
            content_results,
            content_loading,
            entries_query,
            source_filter,
            ..
        }) = agent.active_modal.as_mut()
        {
            notice = PickerSurface {
                entries,
                loading,
                lanes,
                state,
                content_results,
                content_loading,
                entries_query,
                source_filter: *source_filter,
                grouped: true,
                current_repo,
            }
            .native_failed(error_notice.clone(), is_search, chat_mode);
            handled = true;
        }
    }
    if !handled {
        let current_repo = repo_name_from_cwd(&app.cwd.to_string_lossy());
        notice = PickerSurface {
            entries: &mut app.session_picker_entries,
            loading: &mut app.session_picker_loading,
            lanes: &mut app.session_picker_lanes,
            state: &mut app.session_picker_state,
            content_results: &mut app.session_picker_content_results,
            content_loading: &mut app.session_picker_content_loading,
            entries_query: &mut app.session_picker_entries_query,
            source_filter: app.session_picker_source_filter,
            grouped: app.session_picker_grouped,
            current_repo,
        }
        .native_failed(error_notice, is_search, chat_mode);
    }
    if let Some(notice) = notice {
        app.show_toast(&notice);
    }
    vec![]
}

pub(in crate::app::dispatch) fn handle_foreign_sessions_scanned(
    app: &mut AppView,
    scanned: Vec<SessionPickerEntry>,
    seq: u64,
) -> Vec<Effect> {
    if app.chat_mode || seq != app.foreign_session_scan_seq {
        return vec![];
    }
    app.session_picker_detail_generation += 1;
    let mut scanned = Some(scanned);
    let mut notice = None;
    let mut handled = false;
    if let Some(agent) = get_active_agent_mut(app) {
        let current_repo = repo_name_from_cwd(&agent.session.cwd.to_string_lossy());
        if let Some(ActiveModal::SessionPicker {
            entries,
            loading,
            lanes,
            state,
            content_results,
            content_loading,
            entries_query,
            source_filter,
            ..
        }) = agent.active_modal.as_mut()
            && lanes.foreign_loading
        {
            handled = true;
            notice = PickerSurface {
                entries,
                loading,
                lanes,
                state,
                content_results,
                content_loading,
                entries_query,
                source_filter: *source_filter,
                grouped: true,
                current_repo,
            }
            .foreign_loaded(scanned.take().unwrap_or_default());
        }
    }
    if !handled && app.session_picker_lanes.foreign_loading {
        let current_repo = repo_name_from_cwd(&app.cwd.to_string_lossy());
        notice = PickerSurface {
            entries: &mut app.session_picker_entries,
            loading: &mut app.session_picker_loading,
            lanes: &mut app.session_picker_lanes,
            state: &mut app.session_picker_state,
            content_results: &mut app.session_picker_content_results,
            content_loading: &mut app.session_picker_content_loading,
            entries_query: &mut app.session_picker_entries_query,
            source_filter: app.session_picker_source_filter,
            grouped: app.session_picker_grouped,
            current_repo,
        }
        .foreign_loaded(scanned.unwrap_or_default());
    }
    if let Some(notice) = notice {
        app.show_toast(&notice);
    }
    vec![]
}

pub(in crate::app::dispatch) fn invalidate_foreign_picker(app: &mut AppView) {
    app.foreign_session_scan_seq += 1;
    app.foreign_scan_coordinator
        .begin_request(app.foreign_session_scan_seq);
    app.session_picker_lanes = Default::default();
    app.session_picker_detail_generation += 1;
}
