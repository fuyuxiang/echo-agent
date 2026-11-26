mod cursor;
mod envelope;
mod facets;
mod row;
use crate::agent::session_registry_client::SessionRegistryClient;
use crate::remote::{ConvError, ConvQuery, ConversationsClient};
pub use crate::session::merge::CwdScope;
use agent_client_protocol as acp;
use cursor::{CompositeCursor, ConvLane, Paginated, merge_and_paginate};
pub use envelope::{FacetMap, FacetValue, SessionKind, SessionMetaEnvelope};
pub use facets::{
    BRANCH_FACET_KEY, BranchFacet, CWD_FACET_KEY, CwdFacet, FacetProvider, FacetRegistry,
    FacetSummary, FacetSummaryKey, FacetSummaryValue, GIT_ROOT_FACET_KEY, GitRootFacet,
    KIND_FACET_KEY, KindFacet, NormalizedItem, Pushdown, REPO_FACET_KEY, RepoFacet,
    SOURCE_WORKSPACE_FACET_KEY, STARRED_FACET_KEY, SourceQuery, SourceWorkspaceFacet, StarredFacet,
    WORKSPACE_FACET_KEY, WORKTREE_FACET_KEY, WorkspaceFacet, WorktreeFacet, build_facet_registry,
};
pub use row::{
    ExtSupersetRow, RowMeta, SessionInfo, UnifiedRow, conversation_to_row, merged_session_to_row,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::LazyLock;
pub const DEFAULT_LIMIT: usize = 30;
const CONV_PAGE_HEADROOM: usize = 5;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialReason {
    Timeout,
    Error,
    NoOauth,
}
impl PartialReason {
    fn as_str(self) -> &'static str {
        match self {
            PartialReason::Timeout => "timeout",
            PartialReason::Error => "error",
            PartialReason::NoOauth => "no_oauth",
        }
    }
}
static FACET_REGISTRY: LazyLock<FacetRegistry> = LazyLock::new(build_facet_registry);
pub(crate) fn facet_registry() -> &'static FacetRegistry {
    &FACET_REGISTRY
}
/// Hard-off in release builds so they can't enable the
/// conversations lane via env.
pub(crate) fn conversations_lane_enabled() -> bool {
    false
}
/// Env lane (desktop `ECHO_AGENT_SESSION_LIST_CONVERSATIONS`) OR process-wide
/// `--chat` (`ECHO_AGENT_CHAT_MODE`); hard-off in release builds.
/// The single predicate `MvpAgent::conversations_client()` keys on.
pub fn conversations_lane_active() -> bool {
    conversations_lane_enabled() || crate::agent::chat_modes::process_chat_mode_enabled()
}
/// Parse `echo.agent/session/list` params and, under process-wide chat mode, force
/// the conversations-only `kind` facet (see [`force_kind_chat`]).
///
/// Client-sent `kind` of `chat`/`build` is honored only behind
/// `feature = "local-workspace"` (pager welcome Local history). Chat-only
/// Desktop/ACP agents keep the force-rewrite so `kind: ["build"]` cannot
/// surface Build rows.
pub fn parse_list_req(raw: &str) -> Result<ListReq, serde_json::Error> {
    let mut req: ListReq = serde_json::from_str(raw)?;
    if crate::agent::chat_modes::process_chat_mode_enabled() {
        let honor_client_kind = cfg!(feature = "local-workspace") && client_sent_kind_filter(&req);
        if !honor_client_kind {
            force_kind_chat(&mut req);
        }
    }
    Ok(req)
}
fn cwd_scope_from_allow_relax<'de, D>(deserializer: D) -> Result<CwdScope, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(if bool::deserialize(deserializer)? {
        CwdScope::RelaxIfEmpty
    } else {
        CwdScope::WithSiblings
    })
}
fn client_sent_kind_filter(req: &ListReq) -> bool {
    let Some(kind) = req
        .meta
        .as_ref()
        .and_then(|m| m.get("echo.agent/facetFilters"))
        .and_then(|f| f.get("kind"))
    else {
        return false;
    };
    match kind {
        serde_json::Value::Array(arr) if !arr.is_empty() => arr
            .iter()
            .any(|v| matches!(v.as_str(), Some("chat" | "build"))),
        serde_json::Value::String(s) if s == "chat" || s == "build" => true,
        _ => false,
    }
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListReq {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    /// Which directories the listing draws from. The wire carries the original
    /// `allowRelax` boolean; `Only` is reachable only in code (ACP
    /// `session/list`), so "exact" and "relax" cannot be requested together.
    /// A relaxed response sets `_meta["echo.agent/listScope"]`, re-evaluated per page.
    #[serde(
        default,
        rename = "allowRelax",
        deserialize_with = "cwd_scope_from_allow_relax"
    )]
    pub cwd_scope: CwdScope,
    #[serde(default, rename = "_meta")]
    pub meta: Option<serde_json::Value>,
}
/// Directory scope the returned sessions were drawn from. Wire form is the
/// `as_str` value (`echo.agent/listScope`), so no serde derive is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListScope {
    /// Scoped to the request cwd.
    #[default]
    Cwd,
    /// Relaxed to the cwd's repo when the cwd itself had no sessions.
    Repo,
    /// Relaxed to all directories when the cwd is not a git repo.
    All,
}
impl ListScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cwd => "cwd",
            Self::Repo => "repo",
            Self::All => "all",
        }
    }
    /// True when the scope relaxed past the cwd, to the repo or to all directories.
    pub const fn is_relaxed(self) -> bool {
        !matches!(self, Self::Cwd)
    }
}
pub struct UnifiedListResult {
    pub rows: Vec<UnifiedRow>,
    pub next_cursor: Option<String>,
    pub facets: FacetSummary,
    pub conversations_partial: Option<PartialReason>,
    /// Directory scope `rows` were drawn from; see [`ListReq::allow_relax`].
    pub scope: ListScope,
}
#[derive(Debug, Default)]
struct ParsedMeta {
    facet_filters: BTreeMap<String, Vec<serde_json::Value>>,
    query: Option<String>,
    limit: Option<usize>,
}
impl ParsedMeta {
    fn parse(meta: Option<&serde_json::Value>) -> Self {
        let Some(meta) = meta else {
            return Self::default();
        };
        let facet_filters = meta
            .get("echo.agent/facetFilters")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(k, v)| (k.clone(), value_list(v)))
                    .collect()
            })
            .unwrap_or_default();
        let query = meta
            .get("echo.agent/query")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let limit = meta
            .get("echo.agent/limit")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize);
        Self {
            facet_filters,
            query,
            limit,
        }
    }
}
fn value_list(v: &serde_json::Value) -> Vec<serde_json::Value> {
    match v {
        serde_json::Value::Array(arr) => arr.clone(),
        other => vec![other.clone()],
    }
}
/// Rewrite `req` so the `kind` facet filter is exactly `["chat"]`.
///
/// Used when process chat mode is on **and** the client omitted a recognized
/// `kind` facet (see [`parse_list_req`]). Welcome history sends an explicit
/// `kind` (`chat` / `build`) that must not be rewritten. Other facet filters
/// and `_meta` keys are left untouched.
pub(crate) fn force_kind_chat(req: &mut ListReq) {
    force_kind(req, SessionKind::Chat);
}
/// REPLACES any client-sent `kind` allow-list (a union would re-enable the
/// excluded lanes); every other facet filter and `_meta` key is untouched.
pub(crate) fn force_kind(req: &mut ListReq, kind: SessionKind) {
    let mut meta = match req.meta.take() {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    let mut filters = match meta.remove("echo.agent/facetFilters") {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    filters.insert(
        KIND_FACET_KEY.to_owned(),
        serde_json::json!([kind.as_str()]),
    );
    meta.insert(
        "echo.agent/facetFilters".to_owned(),
        serde_json::Value::Object(filters),
    );
    req.meta = Some(serde_json::Value::Object(meta));
}
pub async fn build_unified_list(
    registry_client: Option<&SessionRegistryClient>,
    conversations_client: Option<&ConversationsClient>,
    mut req: ListReq,
) -> UnifiedListResult {
    if crate::agent::chat_modes::process_chat_mode_enabled() && !client_sent_kind_filter(&req) {
        force_kind_chat(&mut req);
    }
    let reg = facet_registry();
    let ParsedMeta {
        facet_filters,
        query: meta_query,
        limit: meta_limit,
    } = ParsedMeta::parse(req.meta.as_ref());
    let limit = req.limit.or(meta_limit).unwrap_or(DEFAULT_LIMIT);
    let query = req.query.or(meta_query);
    let cursor = CompositeCursor::decode(req.cursor.as_deref());
    let mut source_query = SourceQuery::default();
    reg.apply_pushdown(&facet_filters, &mut source_query);
    let exclude_conversations = excludes_conversations(&facet_filters);
    let exclude_build = excludes_build(&facet_filters);
    let over = crate::session::merge::over_fetch(limit);
    let cwd_scope = req.cwd_scope;
    let can_relax = relax_eligible(RelaxGate {
        opted_in: matches!(req.cwd_scope, CwdScope::RelaxIfEmpty),
        no_facet_filters: facet_filters.is_empty(),
        has_cwd: req.cwd.is_some(),
        is_search: query.is_some(),
    });
    let local_fut = async {
        if exclude_build {
            return LocalLane::default();
        }
        let cwd = req.cwd.as_deref();
        if can_relax {
            let lanes =
                crate::session::merge::fetch_lanes(registry_client, cwd, cwd_scope, None, over)
                    .await;
            let rows = to_rows(
                crate::session::merge::merge(
                    lanes.remote.clone(),
                    lanes.local,
                    None,
                    &lanes.repo_urls,
                    over,
                ),
                reg,
            );
            LocalLane {
                rows,
                relax: Some(RelaxInputs {
                    remote: lanes.remote,
                    repo_urls: lanes.repo_urls,
                }),
            }
        } else {
            let merged = crate::session::merge::fetch_merged(
                registry_client,
                cwd,
                cwd_scope,
                query.as_deref(),
                over,
            )
            .await;
            LocalLane {
                rows: to_rows(merged, reg),
                relax: None,
            }
        }
    };
    let conv_fut = async {
        if exclude_conversations {
            return ConvLane::Skipped;
        }
        let Some(client) = conversations_client else {
            return ConvLane::Skipped;
        };
        let q = ConvQuery {
            page_size: (limit + CONV_PAGE_HEADROOM) as i64,
            page_token: cursor.conv_page_token.clone(),
            search_query: query.clone(),
            workspace_id: source_query.workspace_id.clone(),
        };
        match tokio::time::timeout(
            crate::session::merge::REMOTE_TIMEOUT,
            client.list_conversations(&q),
        )
        .await
        {
            Ok(Ok(page)) => {
                let next_token = page.next_page_token;
                let rows: Vec<UnifiedRow> = page
                    .conversations
                    .into_iter()
                    .map(|c| conversation_to_row(c, reg))
                    .collect();
                let frontier = cursor::conv_frontier(&rows, next_token.is_some());
                ConvLane::Page {
                    rows,
                    next_token,
                    frontier,
                }
            }
            Ok(Err(ConvError::NoOauth)) => ConvLane::Degraded(PartialReason::NoOauth),
            Ok(Err(e)) => {
                tracing::warn!("conversation list failed: {e}");
                ConvLane::Degraded(PartialReason::Error)
            }
            Err(_) => {
                tracing::warn!("conversation list timed out");
                ConvLane::Degraded(PartialReason::Timeout)
            }
        }
    };
    let (
        LocalLane {
            rows: local_rows,
            relax,
        },
        conv_lane,
    ) = tokio::join!(local_fut, conv_fut);
    let (local_rows, scope) = maybe_relax(local_rows, relax, over, reg).await;
    {
        let (conv_lane_status, conv_rows) = match &conv_lane {
            ConvLane::Skipped => ("skipped", 0),
            ConvLane::Degraded(reason) => (reason.as_str(), 0),
            ConvLane::Page { rows, .. } => ("ok", rows.len()),
        };
        tracing::debug!(
            local_lane_skipped = exclude_build,
            local_rows = local_rows.len(),
            conv_lane = conv_lane_status,
            conv_rows,
            "session list lanes"
        );
    }
    let local_rows = reg.apply_in_memory_filters(&facet_filters, local_rows);
    let conv_lane = match conv_lane {
        ConvLane::Page {
            rows,
            next_token,
            frontier,
        } => ConvLane::Page {
            rows: reg.apply_in_memory_filters(&facet_filters, rows),
            next_token,
            frontier,
        },
        other => other,
    };
    let Paginated {
        candidates,
        emit_count,
        next_cursor,
        partial,
    } = merge_and_paginate(local_rows, conv_lane, &cursor, limit);
    let mut rows = candidates;
    rows.truncate(emit_count);
    let facets = reg.summarize_window(&rows);
    UnifiedListResult {
        rows,
        next_cursor: next_cursor.map(|c| c.encode()),
        facets,
        conversations_partial: partial,
        scope,
    }
}
#[derive(Default)]
struct LocalLane {
    rows: Vec<UnifiedRow>,
    relax: Option<RelaxInputs>,
}
struct RelaxInputs {
    remote: Vec<crate::agent::session_registry_client::SessionRecord>,
    repo_urls: Vec<String>,
}
fn to_rows(
    merged: Vec<crate::session::merge::MergedSession>,
    reg: &FacetRegistry,
) -> Vec<UnifiedRow> {
    merged
        .into_iter()
        .map(|m| merged_session_to_row(m, reg))
        .collect()
}
#[derive(Clone, Copy)]
struct RelaxGate {
    opted_in: bool,
    no_facet_filters: bool,
    has_cwd: bool,
    is_search: bool,
}
fn relax_eligible(gate: RelaxGate) -> bool {
    gate.opted_in && gate.no_facet_filters && gate.has_cwd && !gate.is_search
}
/// True when no row has messages (a post-rebuild placeholder counts as empty).
fn lane_has_no_messages(rows: &[UnifiedRow]) -> bool {
    rows.iter().all(|r| r.legacy.num_messages == 0)
}
async fn maybe_relax(
    local_rows: Vec<UnifiedRow>,
    relax: Option<RelaxInputs>,
    over: usize,
    reg: &FacetRegistry,
) -> (Vec<UnifiedRow>, ListScope) {
    let Some(relax) = relax.filter(|_| lane_has_no_messages(&local_rows)) else {
        return (local_rows, ListScope::Cwd);
    };
    let scope = if relax.repo_urls.is_empty() {
        ListScope::All
    } else {
        ListScope::Repo
    };
    let all_local = crate::session::persistence::list_summaries(None)
        .await
        .unwrap_or_else(|e| {
            tracing::debug!("cwd scan failed: {e}");
            Vec::new()
        });
    match relax_rows(relax, all_local, over, reg) {
        Some(relaxed) => {
            tracing::debug!(
                rows = relaxed.len(),
                scope = scope.as_str(),
                "cwd empty; relaxing scope"
            );
            (relaxed, scope)
        }
        None => (local_rows, ListScope::Cwd),
    }
}
/// Re-merge the registry page with a repo-scoped local scan (all directories
/// when the cwd is not a repo); relax only when it reveals a messaged session.
fn relax_rows(
    relax: RelaxInputs,
    all_local: Vec<crate::session::persistence::Summary>,
    over: usize,
    reg: &FacetRegistry,
) -> Option<Vec<UnifiedRow>> {
    let scoped = crate::session::merge::filter_summaries_by_repo(all_local, &relax.repo_urls);
    let rows = to_rows(
        crate::session::merge::merge(relax.remote, scoped, None, &relax.repo_urls, over),
        reg,
    );
    (!lane_has_no_messages(&rows)).then_some(rows)
}
fn excludes_conversations(filters: &BTreeMap<String, Vec<serde_json::Value>>) -> bool {
    match filters.get(KIND_FACET_KEY) {
        Some(allowed) if !allowed.is_empty() => !allowed
            .iter()
            .any(|v| v.as_str() == Some(SessionKind::Chat.as_str())),
        _ => false,
    }
}
/// Mirror of [`excludes_conversations`]: `true` when a non-empty `kind`
/// allow-list does not include `"build"`, so the local lane can be skipped.
fn excludes_build(filters: &BTreeMap<String, Vec<serde_json::Value>>) -> bool {
    match filters.get(KIND_FACET_KEY) {
        Some(allowed) if !allowed.is_empty() => !allowed
            .iter()
            .any(|v| v.as_str() == Some(SessionKind::Build.as_str())),
        _ => false,
    }
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtListResponse {
    pub sessions: Vec<ExtSupersetRow>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta")]
    pub meta: ExtListResponseMeta,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExtListResponseMeta {
    #[serde(rename = "echo.agent/facets")]
    pub facets: FacetSummary,
    #[serde(rename = "echo.agent/partial")]
    pub partial: PartialInfo,
    /// Present only when the listing relaxed beyond the cwd.
    #[serde(
        rename = "echo.agent/listScope",
        skip_serializing_if = "Option::is_none"
    )]
    pub list_scope: Option<&'static str>,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PartialInfo {
    pub conversations: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}
fn list_response_meta(result: &UnifiedListResult) -> ExtListResponseMeta {
    ExtListResponseMeta {
        facets: result.facets.clone(),
        partial: PartialInfo {
            conversations: result.conversations_partial.is_some(),
            reason: result.conversations_partial.map(PartialReason::as_str),
        },
        list_scope: result.scope.is_relaxed().then_some(result.scope.as_str()),
    }
}
pub(crate) fn ext_list_response(result: UnifiedListResult) -> ExtListResponse {
    let meta = list_response_meta(&result);
    ExtListResponse {
        sessions: result
            .rows
            .into_iter()
            .map(UnifiedRow::into_ext_superset)
            .collect(),
        next_cursor: result.next_cursor,
        meta,
    }
}
pub(crate) fn acp_response_meta(result: &UnifiedListResult) -> Option<acp::Meta> {
    to_meta(serde_json::to_value(list_response_meta(result)))
}
pub(super) fn to_meta<E: std::fmt::Display>(
    value: Result<serde_json::Value, E>,
) -> Option<acp::Meta> {
    match value {
        Ok(serde_json::Value::Object(map)) => Some(map),
        Ok(other) => {
            tracing::warn!(kind = ?other, "session list _meta was not an object");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "session list _meta failed to serialize");
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::merge::MergedSession;
    fn local(session_id: &str, updated_at: &str) -> MergedSession {
        MergedSession {
            session_id: session_id.into(),
            summary: "a summary".into(),
            first_prompt: Some("first prompt".into()),
            updated_at: updated_at.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            cwd: "/Users/me/echoagent".into(),
            hostname: Some("devbox".into()),
            source: "local".into(),
            model_id: Some("echo-agent-build".into()),
            num_messages: 7,
            last_active_at: Some(updated_at.into()),
            branch: Some("main".into()),
            repo_name: Some("echoagent".into()),
            worktree_label: Some("wt".into()),
            git_root_dir: Some("/Users/me/echoagent".into()),
            git_remotes: vec!["git@github.com:example/repo.git".into()],
            source_workspace_dir: Some("/Users/me/echo-agent-src".into()),
            last_turn_summary: None,
            last_recap: None,
            session_kind: Some("worktree".into()),
        }
    }
    fn row(session_id: &str, updated_at: &str) -> UnifiedRow {
        merged_session_to_row(local(session_id, updated_at), facet_registry())
    }
    #[test]
    fn ext_superset_preserves_every_legacy_field_and_adds_title_and_meta() {
        let value = serde_json::to_value(row("s1", "2026-06-18T20:10:00Z").into_ext_superset())
            .expect("serialize");
        for field in [
            "sessionId",
            "summary",
            "firstPrompt",
            "updatedAt",
            "createdAt",
            "cwd",
            "hostname",
            "source",
            "modelId",
            "numMessages",
            "lastActiveAt",
            "branch",
            "repoName",
            "worktreeLabel",
        ] {
            assert!(value.get(field).is_some(), "missing legacy field: {field}");
        }
        assert_eq!(value["sessionId"], "s1");
        assert_eq!(value["source"], "local");
        assert_eq!(value["numMessages"], 7);
        assert_eq!(value["title"], "a summary");
        assert_eq!(value["_meta"]["echo.agent/session"]["kind"], "build");
        assert_eq!(value["gitRootDir"], "/Users/me/echoagent");
        assert_eq!(value["gitRemotes"][0], "git@github.com:example/repo.git");
        assert_eq!(value["sourceWorkspaceDir"], "/Users/me/echo-agent-src");
        assert_eq!(value["sessionKind"], "worktree");
    }
    #[test]
    fn facets_carry_kind_and_cwd() {
        let r = row("s1", "2026-06-18T20:10:00Z");
        assert!(matches!(
            r.facets.get(KIND_FACET_KEY),
            Some(FacetValue::One(serde_json::Value::String(k))) if k == "build"
        ));
        assert!(matches!(
            r.facets.get(CWD_FACET_KEY),
            Some(FacetValue::One(serde_json::Value::String(c))) if c == "/Users/me/echoagent"
        ));
    }
    #[test]
    fn bare_session_info_is_minimal_plus_meta() {
        let value =
            serde_json::to_value(row("s1", "2026-06-18T20:10:00Z").into_session_info()).unwrap();
        assert_eq!(value["sessionId"], "s1");
        assert_eq!(value["cwd"], "/Users/me/echoagent");
        assert_eq!(value["title"], "a summary");
        assert_eq!(value["_meta"]["echo.agent/session"]["kind"], "build");
        assert!(value.get("summary").is_none());
        assert!(value.get("source").is_none());
    }
    #[test]
    fn total_order_is_updated_at_desc_then_session_id() {
        let mut rows = [
            row("b", "2026-01-01T00:00:00Z"),
            row("a", "2026-06-01T00:00:00Z"),
            row("c", "2026-06-01T00:00:00Z"),
        ];
        rows.sort_by(super::cursor::cmp_total_order);
        let ids: Vec<&str> = rows.iter().map(|r| r.legacy.session_id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "b"]);
    }
    #[test]
    fn kind_filter_local_keeps_local_rows() {
        let reg = facet_registry();
        let rows = vec![row("s1", "2026-06-01T00:00:00Z")];
        let mut filters = BTreeMap::new();
        filters.insert(KIND_FACET_KEY.to_owned(), vec![serde_json::json!("build")]);
        let kept = reg.apply_in_memory_filters(&filters, rows);
        assert_eq!(kept.len(), 1);
    }
    #[test]
    fn kind_filter_conversation_drops_local_rows() {
        let reg = facet_registry();
        let rows = vec![row("s1", "2026-06-01T00:00:00Z")];
        let mut filters = BTreeMap::new();
        filters.insert(KIND_FACET_KEY.to_owned(), vec![serde_json::json!("chat")]);
        let kept = reg.apply_in_memory_filters(&filters, rows);
        assert!(kept.is_empty());
    }
    #[test]
    fn cwd_filter_is_skipped_in_memory() {
        let reg = facet_registry();
        let rows = vec![row("s1", "2026-06-01T00:00:00Z")];
        let mut filters = BTreeMap::new();
        filters.insert(
            CWD_FACET_KEY.to_owned(),
            vec![serde_json::json!("/some/other/dir")],
        );
        let kept = reg.apply_in_memory_filters(&filters, rows);
        assert_eq!(kept.len(), 1);
    }
    #[test]
    fn empty_allow_list_is_a_no_op() {
        let reg = facet_registry();
        let rows = vec![row("s1", "2026-06-01T00:00:00Z")];
        let mut filters = BTreeMap::new();
        filters.insert(KIND_FACET_KEY.to_owned(), Vec::new());
        let kept = reg.apply_in_memory_filters(&filters, rows);
        assert_eq!(kept.len(), 1);
    }
    #[test]
    fn parsed_meta_reads_facet_filters_query_and_limit() {
        let meta = serde_json::json!({
            "echo.agent/facetFilters": { "kind": ["build"], "starred": true },
            "echo.agent/query": "antelope",
            "echo.agent/limit": 5,
        });
        let parsed = ParsedMeta::parse(Some(&meta));
        assert_eq!(parsed.query.as_deref(), Some("antelope"));
        assert_eq!(parsed.limit, Some(5));
        assert_eq!(
            parsed.facet_filters.get("kind"),
            Some(&vec![serde_json::json!("build")])
        );
        assert_eq!(
            parsed.facet_filters.get("starred"),
            Some(&vec![serde_json::json!(true)])
        );
    }
    fn kind_filter(values: &[&str]) -> BTreeMap<String, Vec<serde_json::Value>> {
        let mut filters = BTreeMap::new();
        filters.insert(
            KIND_FACET_KEY.to_owned(),
            values.iter().map(|v| serde_json::json!(v)).collect(),
        );
        filters
    }
    #[test]
    fn excludes_build_mirrors_excludes_conversations() {
        assert!(excludes_build(&kind_filter(&["chat"])));
        assert!(!excludes_conversations(&kind_filter(&["chat"])));
        assert!(!excludes_build(&kind_filter(&["build"])));
        assert!(excludes_conversations(&kind_filter(&["build"])));
        assert!(!excludes_build(&kind_filter(&["build", "chat"])));
        assert!(!excludes_conversations(&kind_filter(&["build", "chat"])));
        assert!(!excludes_build(&kind_filter(&[])));
        assert!(!excludes_conversations(&kind_filter(&[])));
        assert!(!excludes_build(&BTreeMap::new()));
        assert!(!excludes_conversations(&BTreeMap::new()));
    }
    /// The forced `kind` REPLACES a client-sent `kind: ["build"]` (never
    /// unions), so the local lane stays excluded.
    #[test]
    fn forced_kind_replaces_client_build_filter() {
        let mut req = ListReq {
            meta: Some(serde_json::json!({
                "echo.agent/facetFilters": { "kind": ["build"] },
            })),
            ..ListReq::default()
        };
        force_kind_chat(&mut req);
        let parsed = ParsedMeta::parse(req.meta.as_ref());
        assert_eq!(
            parsed.facet_filters.get(KIND_FACET_KEY),
            Some(&vec![serde_json::json!("chat")]),
            "forced kind must replace the client filter, not union with it"
        );
        assert!(excludes_build(&parsed.facet_filters));
        assert!(!excludes_conversations(&parsed.facet_filters));
    }
    #[test]
    fn forced_kind_preserves_other_facets() {
        let mut req = ListReq {
            meta: Some(serde_json::json!({
                "echo.agent/facetFilters": { "kind": ["build"], "starred": [true], "workspace": ["w1"] },
                "echo.agent/query": "antelope",
                "echo.agent/limit": 5,
            })),
            ..ListReq::default()
        };
        force_kind_chat(&mut req);
        let parsed = ParsedMeta::parse(req.meta.as_ref());
        assert_eq!(
            parsed.facet_filters.get(KIND_FACET_KEY),
            Some(&vec![serde_json::json!("chat")])
        );
        assert_eq!(
            parsed.facet_filters.get("starred"),
            Some(&vec![serde_json::json!(true)])
        );
        assert_eq!(
            parsed.facet_filters.get("workspace"),
            Some(&vec![serde_json::json!("w1")])
        );
        assert_eq!(parsed.query.as_deref(), Some("antelope"));
        assert_eq!(parsed.limit, Some(5));
    }
    #[test]
    fn forced_kind_creates_facet_filters_when_meta_absent() {
        let mut req = ListReq::default();
        force_kind_chat(&mut req);
        let parsed = ParsedMeta::parse(req.meta.as_ref());
        assert_eq!(
            parsed.facet_filters.get(KIND_FACET_KEY),
            Some(&vec![serde_json::json!("chat")])
        );
    }
    fn echo_agent_auth_manager(dir: &std::path::Path) -> std::sync::Arc<crate::auth::AuthManager> {
        let am = std::sync::Arc::new(crate::auth::AuthManager::new(
            dir,
            crate::auth::EchoAgentComConfig::default(),
        ));
        am.hot_swap(crate::auth::EchoAgentAuth {
            auth_mode: crate::auth::AuthMode::Oidc,
            oidc_issuer: Some(crate::auth::echo_agent_oauth2_issuer().to_owned()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..crate::auth::EchoAgentAuth::test_default()
        });
        am
    }
    /// Minimal HTTP/1.1 responder serving `body` as JSON to every request.
    async fn spawn_conversations_stub(body: String) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 8192];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        addr
    }
    /// A client-sent `kind: ["build"]` rewritten by [`force_kind_chat`]
    /// yields conversations only.
    #[tokio::test]
    #[serial_test::serial]
    async fn forced_kind_serves_conversations_only() {
        let addr = spawn_conversations_stub(
                serde_json::json!({
                "conversations": [
                    { "conversationId": "c1", "title": "Hello", "modifyTime": "2026-07-01T00:00:00Z" },
                    { "conversationId": "c2", "title": "", "modifyTime": "2026-07-02T00:00:00Z" },
                ],
            })
                    .to_string(),
            )
            .await;
        let _env = echo_agent_test_support::EnvGuard::set(
            "ECHO_AGENT_CONVERSATIONS_BASE_URL",
            format!("http://{addr}"),
        );
        let home = tempfile::tempdir().expect("tempdir");
        let client = ConversationsClient::new(echo_agent_auth_manager(home.path()));
        let mut req = ListReq {
            meta: Some(serde_json::json!({
                "echo.agent/facetFilters": { "kind": ["build"] },
            })),
            ..ListReq::default()
        };
        force_kind_chat(&mut req);
        let result = build_unified_list(None, Some(&client), req).await;
        let ids: Vec<&str> = result
            .rows
            .iter()
            .map(|r| r.legacy.session_id.as_str())
            .collect();
        assert_eq!(ids, ["c2", "c1"], "conversations only, newest first");
        assert!(
            result
                .rows
                .iter()
                .all(|r| r.legacy.source == "conversation"),
            "no build row may survive the forced kind filter"
        );
        assert_eq!(result.conversations_partial, None);
    }
    /// A degraded conversations lane (no OAuth) surfaces through
    /// `conversations_partial` instead of failing the list.
    #[tokio::test]
    #[serial_test::serial]
    async fn degraded_conversations_lane_reports_no_oauth() {
        let home = tempfile::tempdir().expect("tempdir");
        let auth = std::sync::Arc::new(crate::auth::AuthManager::new(
            home.path(),
            crate::auth::EchoAgentComConfig::default(),
        ));
        auth.set_devbox_env_for_test(false);
        let client = ConversationsClient::new(auth);
        let mut req = ListReq::default();
        force_kind_chat(&mut req);
        let result = build_unified_list(None, Some(&client), req).await;
        assert!(result.rows.is_empty());
        assert_eq!(result.conversations_partial, Some(PartialReason::NoOauth));
    }
    /// Build-mode canary: with no conversations client the lane is skipped —
    /// not degraded.
    #[tokio::test]
    async fn non_chat_list_without_client_skips_conversations_lane() {
        let req = ListReq {
            cwd: Some("/nonexistent/unified-list-canary".into()),
            ..ListReq::default()
        };
        let result = build_unified_list(None, None, req).await;
        assert_eq!(
            result.conversations_partial, None,
            "no client ⇒ lane skipped, never reported as degraded"
        );
        assert!(result.rows.is_empty());
    }
    /// Desktop env lane stays env-gated; process chat mode is feature-gated.
    #[test]
    #[serial_test::serial]
    fn conversations_lane_env_gating_matrix() {
        {
            let _off =
                echo_agent_test_support::EnvGuard::unset("ECHO_AGENT_SESSION_LIST_CONVERSATIONS");
            assert!(!conversations_lane_enabled());
        }
        {
            let _on = echo_agent_test_support::EnvGuard::set(
                "ECHO_AGENT_SESSION_LIST_CONVERSATIONS",
                "1",
            );
            assert_eq!(conversations_lane_enabled(), false);
        }
        {
            let _off = echo_agent_test_support::EnvGuard::set(
                "ECHO_AGENT_SESSION_LIST_CONVERSATIONS",
                "0",
            );
            assert!(!conversations_lane_enabled());
        }
    }
    /// Truth table for `conversations_lane_active`: desktop env lane OR
    /// process chat mode, hard-off in release builds.
    #[test]
    #[serial_test::serial]
    fn conversations_lane_active_truth_table() {
        use crate::agent::chat_modes::ECHO_AGENT_CHAT_MODE_ENV;
        let _chat_off = echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_MODE_ENV);
        let _desktop_off =
            echo_agent_test_support::EnvGuard::unset("ECHO_AGENT_SESSION_LIST_CONVERSATIONS");
        assert!(
            !conversations_lane_active(),
            "no env ⇒ lane off (Build-mode default)"
        );
        {
            let _desktop = echo_agent_test_support::EnvGuard::set(
                "ECHO_AGENT_SESSION_LIST_CONVERSATIONS",
                "1",
            );
            assert_eq!(conversations_lane_active(), false);
        }
        {
            let _chat = echo_agent_test_support::EnvGuard::set(ECHO_AGENT_CHAT_MODE_ENV, "1");
            assert_eq!(
                conversations_lane_active(),
                false,
                "process chat mode must enable the lane (chat feature only)"
            );
        }
    }
    /// Retired chat-mode flags must not rewrite or discard client metadata.
    #[test]
    #[serial_test::serial]
    fn parse_list_req_preserves_client_meta_when_chat_mode_is_retired() {
        use crate::agent::chat_modes::ECHO_AGENT_CHAT_MODE_ENV;
        let cases = [
            (serde_json::json!({}), None),
            (
                serde_json::json!({ "_meta": { "clientField": "keep", "echo.agent/facetFilters": { "kind": ["build"], "starred": [true] } } }),
                Some(vec![serde_json::json!("build")]),
            ),
            (
                serde_json::json!({ "_meta": { "echo.agent/facetFilters": { "kind": [] } } }),
                Some(vec![]),
            ),
            (
                serde_json::json!({ "_meta": { "echo.agent/facetFilters": { "kind": null } } }),
                Some(vec![serde_json::Value::Null]),
            ),
            (
                serde_json::json!({ "_meta": { "echo.agent/facetFilters": { "kind": ["other"] } } }),
                Some(vec![serde_json::json!("other")]),
            ),
        ];
        for flag in [None, Some("0"), Some("1")] {
            let _guard = match flag {
                Some(value) => {
                    echo_agent_test_support::EnvGuard::set(ECHO_AGENT_CHAT_MODE_ENV, value)
                }
                None => echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_MODE_ENV),
            };
            for (raw, expected_kind) in &cases {
                let expected: ListReq = serde_json::from_value(raw.clone()).expect("request");
                let req = parse_list_req(&raw.to_string()).expect("parse");
                assert_eq!(req.meta, expected.meta, "legacy flag {flag:?}: {raw}");
                let parsed = ParsedMeta::parse(req.meta.as_ref());
                assert_eq!(
                    parsed.facet_filters.get(KIND_FACET_KEY),
                    expected_kind.as_ref(),
                    "client kind survives retired flag {flag:?}: {raw}"
                );
            }
        }
    }
    /// Wire pin for the cross-crate `echo.agent/partial` envelope the pager parses:
    /// the serialized reason strings must not drift (the pager maps unknown
    /// reasons to a generic retry notice, masking a rename).
    #[test]
    fn ext_list_response_serializes_partial_reasons() {
        for (reason, wire) in [
            (PartialReason::NoOauth, "no_oauth"),
            (PartialReason::Timeout, "timeout"),
            (PartialReason::Error, "error"),
        ] {
            let value = serde_json::to_value(ext_list_response(UnifiedListResult {
                rows: Vec::new(),
                next_cursor: None,
                facets: facet_registry().summarize_window(&[]),
                conversations_partial: Some(reason),
                scope: ListScope::Cwd,
            }))
            .expect("serialize");
            assert_eq!(
                value["_meta"]["echo.agent/partial"],
                serde_json::json!({ "conversations": true, "reason": wire })
            );
        }
        let healthy = serde_json::to_value(ext_list_response(UnifiedListResult {
            rows: Vec::new(),
            next_cursor: None,
            facets: facet_registry().summarize_window(&[]),
            conversations_partial: None,
            scope: ListScope::Cwd,
        }))
        .expect("serialize");
        assert_eq!(
            healthy["_meta"]["echo.agent/partial"],
            serde_json::json!({ "conversations": false })
        );
    }
    /// Receive-side wire pin: a field rename would silently drop the pager's
    /// `allowRelax`.
    #[test]
    fn list_req_deserializes_allow_relax_key() {
        let req: ListReq = serde_json::from_str(r#"{"allowRelax": true}"#).expect("parse");
        assert_eq!(req.cwd_scope, CwdScope::RelaxIfEmpty);
        let req: ListReq = serde_json::from_str("{}").expect("parse");
        assert_eq!(req.cwd_scope, CwdScope::WithSiblings);
    }
    /// relax_rows scopes to the cwd's repo and relaxes only on a messaged session.
    #[test]
    fn relax_rows_scopes_to_repo_and_requires_messages() {
        use crate::session::persistence::Summary;
        let this_repo = "git@github.com:example/app.git";
        let repo_url = echo_agent_workspace::session::git::normalize_repo_url(this_repo).unwrap();
        let summary = |id: &str, remote: Option<&str>, num_messages: usize| {
            let mut s = Summary::new(
                &crate::session::info::Info {
                    id: agent_client_protocol::SessionId::new(id),
                    cwd: format!("/elsewhere/{id}"),
                },
                agent_client_protocol::ModelId::new("m"),
            )
            .expect("summary");
            s.num_messages = num_messages;
            s.git_remotes = remote.map(|r| vec![r.to_string()]).unwrap_or_default();
            s
        };
        let relax = || RelaxInputs {
            remote: Vec::new(),
            repo_urls: vec![repo_url.clone()],
        };
        let rows = relax_rows(
            relax(),
            vec![
                summary("mine", Some(this_repo), 4),
                summary("theirs", Some("git@github.com:example/other.git"), 9),
            ],
            30,
            facet_registry(),
        )
        .expect("relaxes onto the same-repo messaged session");
        let ids: Vec<_> = rows.iter().map(|r| r.legacy.session_id.clone()).collect();
        assert_eq!(ids, ["mine"], "only the same-repo session survives");
        assert!(
            relax_rows(
                relax(),
                vec![summary("husk", Some(this_repo), 0)],
                30,
                facet_registry()
            )
            .is_none(),
            "placeholder-only scan keeps the scoped view"
        );
    }
    /// Send-side wire pin: `echo.agent/listScope` present iff the scope relaxed.
    #[test]
    fn ext_list_response_serializes_scope() {
        let result = |scope| UnifiedListResult {
            rows: Vec::new(),
            next_cursor: None,
            facets: facet_registry().summarize_window(&[]),
            conversations_partial: None,
            scope,
        };
        let with =
            serde_json::to_value(ext_list_response(result(ListScope::Repo))).expect("serialize");
        assert_eq!(
            with["_meta"]["echo.agent/listScope"],
            serde_json::json!("repo")
        );
        let without =
            serde_json::to_value(ext_list_response(result(ListScope::Cwd))).expect("serialize");
        assert!(without["_meta"].get("echo.agent/listScope").is_none());
    }
}
