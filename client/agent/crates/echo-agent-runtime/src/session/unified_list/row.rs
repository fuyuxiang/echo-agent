use std::path::PathBuf;

use agent_client_protocol as acp;
use serde::Serialize;

use super::envelope::{FacetMap, SessionKind, SessionMetaEnvelope};
use super::facets::{FacetRegistry, NormalizedItem};
use crate::remote::Conversation;
use crate::session::merge::MergedSession;

#[derive(Debug, Clone)]
pub struct UnifiedRow {
    pub kind: SessionKind,
    pub legacy: MergedSession,
    pub title: String,
    pub updated_at: Option<String>,
    pub facets: FacetMap,
}

impl UnifiedRow {
    fn envelope(kind: SessionKind, facets: FacetMap) -> RowMeta {
        RowMeta {
            session: SessionMetaEnvelope { kind, facets },
        }
    }

    pub(crate) fn into_ext_superset(self) -> ExtSupersetRow {
        let UnifiedRow {
            kind,
            legacy,
            title,
            facets,
            updated_at: _,
        } = self;
        ExtSupersetRow {
            legacy,
            title,
            meta: Self::envelope(kind, facets),
        }
    }

    pub(crate) fn into_session_info(self) -> SessionInfo {
        let UnifiedRow {
            kind,
            legacy,
            title,
            updated_at,
            facets,
        } = self;
        SessionInfo {
            session_id: legacy.session_id,
            cwd: legacy.cwd,
            title: (!title.is_empty()).then_some(title),
            updated_at,
            meta: Self::envelope(kind, facets),
        }
    }

    pub(super) fn sort_timestamp(&self) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        self.updated_at
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
    }
}

pub fn merged_session_to_row(m: MergedSession, reg: &FacetRegistry) -> UnifiedRow {
    let facets = reg.extract_all(&NormalizedItem::from_merged(&m));
    let title = m.summary.clone();
    let updated_at = effective_local_ts(&m);
    UnifiedRow {
        kind: SessionKind::Build,
        legacy: m,
        title,
        updated_at,
        facets,
    }
}

pub fn conversation_to_row(c: Conversation, reg: &FacetRegistry) -> UnifiedRow {
    let facets = reg.extract_all(&NormalizedItem::from_conversation(&c));
    let Conversation {
        conversation_id,
        title,
        modify_time,
        create_time,
        ..
    } = c;
    let legacy = MergedSession {
        session_id: conversation_id,
        summary: title.clone(),
        first_prompt: None,
        updated_at: modify_time.as_deref().unwrap_or_default().to_owned(),
        created_at: create_time.unwrap_or_default(),
        cwd: String::new(),
        hostname: None,
        source: "conversation".to_string(),
        model_id: None,
        num_messages: 0,
        last_active_at: modify_time.clone(),
        branch: None,
        repo_name: None,
        worktree_label: None,
        git_root_dir: None,
        git_remotes: Vec::new(),
        source_workspace_dir: None,
        last_turn_summary: None,
        last_recap: None,
        session_kind: None,
    };
    UnifiedRow {
        kind: SessionKind::Chat,
        legacy,
        title,
        updated_at: modify_time,
        facets,
    }
}

fn effective_local_ts(m: &MergedSession) -> Option<String> {
    m.last_active_at
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .or_else(|| chrono::DateTime::parse_from_rfc3339(&m.updated_at).ok())
        .map(|dt| dt.to_rfc3339())
}

#[derive(Debug, Clone, Serialize)]
pub struct RowMeta {
    #[serde(rename = "echo.agent/session")]
    pub session: SessionMetaEnvelope,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtSupersetRow {
    #[serde(flatten)]
    pub legacy: MergedSession,
    pub title: String,
    #[serde(rename = "_meta")]
    pub meta: RowMeta,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(rename = "_meta")]
    pub meta: RowMeta,
}

/// The ACP schema types `cwd` as an absolute path, so a row without one has no
/// representation there.
#[derive(Debug, PartialEq, Eq)]
pub struct CwdNotAbsolute;

impl TryFrom<SessionInfo> for acp::SessionInfo {
    type Error = CwdNotAbsolute;

    fn try_from(info: SessionInfo) -> Result<Self, Self::Error> {
        if !PathBuf::from(&info.cwd).is_absolute() {
            return Err(CwdNotAbsolute);
        }
        let SessionInfo {
            session_id,
            cwd,
            title,
            updated_at,
            meta,
        } = info;
        let meta = super::to_meta(serde_json::to_value(&meta));
        Ok(
            acp::SessionInfo::new(acp::SessionId::new(session_id), PathBuf::from(cwd))
                .title(title)
                .updated_at(updated_at)
                .meta(meta),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::unified_list::facet_registry;

    #[test]
    fn conversation_row_uses_conversation_id_as_session_id() {
        let c = Conversation {
            conversation_id: "conv_abc123".into(),
            title: "Compare GPU vendors".into(),
            modify_time: Some("2026-06-18T18:02:00Z".into()),
            create_time: Some("2026-06-18T17:30:00Z".into()),
            ..Conversation::default()
        };
        let row = conversation_to_row(c, facet_registry());
        assert_eq!(row.legacy.session_id, "conv_abc123");
        assert_eq!(row.kind, SessionKind::Chat);
        assert_eq!(row.legacy.source, "conversation");
        assert_eq!(row.legacy.cwd, "");

        let ext = serde_json::to_value(row.clone().into_ext_superset()).unwrap();
        assert_eq!(ext["sessionId"], "conv_abc123");
        assert_eq!(ext["cwd"], "");
        assert_eq!(ext["source"], "conversation");
        assert_eq!(ext["_meta"]["echo.agent/session"]["kind"], "chat");
        // Chat rows have no local git enrichment (fields omitted).
        assert!(ext.get("gitRootDir").is_none());
        assert!(ext.get("gitRemotes").is_none());
        assert!(ext.get("sourceWorkspaceDir").is_none());
        assert!(ext.get("sessionKind").is_none());

        let bare = serde_json::to_value(row.into_session_info()).unwrap();
        assert_eq!(bare["sessionId"], "conv_abc123");
    }

    #[test]
    fn acp_conversion_preserves_the_row() {
        let merged = MergedSession {
            session_id: "sess_abc123".into(),
            summary: "Implement session list API".into(),
            updated_at: "2026-10-29T14:22:15Z".into(),
            cwd: "/Users/me/echoagent".into(),
            ..MergedSession::default()
        };
        let info = merged_session_to_row(merged, facet_registry()).into_session_info();
        let row = serde_json::to_value(&info).unwrap();
        let acp_row =
            serde_json::to_value(acp::SessionInfo::try_from(info).expect("absolute cwd")).unwrap();

        assert_eq!(row, acp_row);
    }

    #[test]
    fn acp_conversion_rejects_a_row_without_an_absolute_cwd() {
        let merged = MergedSession {
            session_id: "sess_remote".into(),
            cwd: String::new(),
            ..MergedSession::default()
        };
        let info = merged_session_to_row(merged, facet_registry()).into_session_info();
        assert_eq!(acp::SessionInfo::try_from(info), Err(CwdNotAbsolute));
    }

    #[test]
    fn conversation_missing_modify_time_still_resumable() {
        let c = Conversation {
            conversation_id: "conv_no_time".into(),
            title: "Untitled".into(),
            ..Conversation::default()
        };
        let row = conversation_to_row(c, facet_registry());
        assert_eq!(row.legacy.session_id, "conv_no_time");
        assert!(row.updated_at.is_none());
        assert_eq!(row.legacy.updated_at, "");
    }
}
