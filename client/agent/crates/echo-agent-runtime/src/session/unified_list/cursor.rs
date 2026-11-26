use std::cmp::{Ordering, Reverse};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use super::PartialReason;
use super::envelope::SessionKind;
use super::row::UnifiedRow;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct CompositeCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<BoundaryKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conv_page_token: Option<String>,
    #[serde(default)]
    pub conv_page_drained: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BoundaryKey {
    pub updated_at: String,
    pub kind: SessionKind,
    pub session_id: String,
}

impl CompositeCursor {
    pub(super) fn decode(raw: Option<&str>) -> Self {
        raw.filter(|s| !s.is_empty())
            .and_then(|s| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(s)
                    .ok()
            })
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub(super) fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    }
}

pub(super) enum ConvLane {
    Skipped,
    Degraded(PartialReason),
    Page {
        rows: Vec<UnifiedRow>,
        next_token: Option<String>,
        frontier: Option<BoundaryKey>,
    },
}

pub(super) fn conv_frontier(raw_rows: &[UnifiedRow], has_more: bool) -> Option<BoundaryKey> {
    if !has_more {
        return None;
    }
    raw_rows
        .iter()
        .max_by(|a, b| cmp_total_order(a, b))
        .map(boundary_of)
}

pub(super) struct Paginated {
    pub candidates: Vec<UnifiedRow>,
    pub emit_count: usize,
    pub next_cursor: Option<CompositeCursor>,
    pub partial: Option<PartialReason>,
}

pub(super) fn merge_and_paginate(
    local: Vec<UnifiedRow>,
    conv: ConvLane,
    cursor: &CompositeCursor,
    limit: usize,
) -> Paginated {
    let (conv_rows, conv_next_token, conv_fetched, conv_frontier, partial) = match conv {
        ConvLane::Skipped => (Vec::new(), None, false, None, None),
        ConvLane::Degraded(reason) => (Vec::new(), None, false, None, Some(reason)),
        ConvLane::Page {
            rows,
            next_token,
            frontier,
        } => (rows, next_token, true, frontier, None),
    };

    let mut keyed: Vec<(SortKey, UnifiedRow)> = local
        .into_iter()
        .chain(conv_rows)
        .map(|row| (row_sort_key(&row), row))
        .collect();

    if let Some(boundary) = &cursor.boundary {
        let bkey = boundary_sort_key(boundary);
        keyed.retain(|(k, _)| k.cmp(&bkey) == Ordering::Greater);
    }

    keyed.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut emit_count = keyed.len().min(limit);
    if let Some(frontier) = &conv_frontier {
        let fkey = boundary_sort_key(frontier);
        let frontier_count = keyed
            .iter()
            .take_while(|(k, _)| k.cmp(&fkey) != Ordering::Greater)
            .count();
        emit_count = emit_count.min(frontier_count);
    }
    let new_boundary = (emit_count > 0).then(|| boundary_of(&keyed[emit_count - 1].1));

    let tail = &keyed[emit_count..];
    let local_has_more = tail.iter().any(|(_, r)| r.kind == SessionKind::Build);
    let conv_in_tail = tail.iter().any(|(_, r)| r.kind == SessionKind::Chat);

    let (next_conv_token, next_conv_drained, conv_has_more) = if conv_fetched {
        if conv_in_tail {
            (cursor.conv_page_token.clone(), false, true)
        } else {
            let has_more = conv_next_token.is_some();
            (conv_next_token, true, has_more)
        }
    } else if partial.is_some() && cursor.conv_page_token.is_some() && new_boundary.is_some() {
        (
            cursor.conv_page_token.clone(),
            cursor.conv_page_drained,
            true,
        )
    } else {
        (
            cursor.conv_page_token.clone(),
            cursor.conv_page_drained,
            false,
        )
    };

    let next_cursor = (local_has_more || conv_has_more).then(|| CompositeCursor {
        boundary: new_boundary.or_else(|| cursor.boundary.clone()),
        conv_page_token: next_conv_token,
        conv_page_drained: next_conv_drained,
    });

    let candidates: Vec<UnifiedRow> = keyed.into_iter().map(|(_, row)| row).collect();

    Paginated {
        candidates,
        emit_count,
        next_cursor,
        partial,
    }
}

type SortKey = (
    Reverse<Option<chrono::DateTime<chrono::FixedOffset>>>,
    SessionKind,
    String,
);

fn row_sort_key(row: &UnifiedRow) -> SortKey {
    (
        Reverse(row.sort_timestamp()),
        row.kind,
        row.legacy.session_id.clone(),
    )
}

fn boundary_sort_key(boundary: &BoundaryKey) -> SortKey {
    (
        Reverse(parse_ts(&boundary.updated_at)),
        boundary.kind,
        boundary.session_id.clone(),
    )
}

fn boundary_of(row: &UnifiedRow) -> BoundaryKey {
    BoundaryKey {
        updated_at: row.updated_at.clone().unwrap_or_default(),
        kind: row.kind,
        session_id: row.legacy.session_id.clone(),
    }
}

fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(s).ok()
}

pub(super) fn timestamp_desc(
    a: Option<chrono::DateTime<chrono::FixedOffset>>,
    b: Option<chrono::DateTime<chrono::FixedOffset>>,
) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.cmp(&x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(super) fn cmp_total_order(a: &UnifiedRow, b: &UnifiedRow) -> Ordering {
    timestamp_desc(a.sort_timestamp(), b.sort_timestamp())
        .then_with(|| a.kind.cmp(&b.kind))
        .then_with(|| a.legacy.session_id.cmp(&b.legacy.session_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::Conversation;
    use crate::session::merge::MergedSession;
    use crate::session::unified_list::{
        conversation_to_row, facet_registry, merged_session_to_row,
    };
    use std::collections::BTreeSet;

    fn local(id: &str, ts: &str) -> UnifiedRow {
        let m = MergedSession {
            session_id: id.into(),
            summary: "s".into(),
            first_prompt: None,
            updated_at: ts.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            cwd: "/x".into(),
            hostname: None,
            source: "local".into(),
            model_id: None,
            num_messages: 1,
            last_active_at: Some(ts.into()),
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
        merged_session_to_row(m, facet_registry())
    }

    fn conv(id: &str, ts: &str) -> UnifiedRow {
        let c = Conversation {
            conversation_id: id.into(),
            title: "t".into(),
            modify_time: Some(ts.into()),
            ..Conversation::default()
        };
        conversation_to_row(c, facet_registry())
    }

    struct ConvSource {
        rows: Vec<UnifiedRow>,
        page_size: usize,
    }

    impl ConvSource {
        fn new(mut rows: Vec<UnifiedRow>, page_size: usize) -> Self {
            rows.sort_by(cmp_total_order);
            Self { rows, page_size }
        }

        fn page(&self, token: Option<&str>) -> ConvLane {
            if self.rows.is_empty() {
                return ConvLane::Page {
                    rows: Vec::new(),
                    next_token: None,
                    frontier: None,
                };
            }
            let idx = token
                .and_then(|t| t.strip_prefix('p'))
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            let start = idx * self.page_size;
            let end = (start + self.page_size).min(self.rows.len());
            let rows = self.rows.get(start..end).unwrap_or(&[]).to_vec();
            let next_token = (end < self.rows.len()).then(|| format!("p{}", idx + 1));
            let frontier = conv_frontier(&rows, next_token.is_some());
            ConvLane::Page {
                rows,
                next_token,
                frontier,
            }
        }
    }

    fn walk_all(local_window: &[UnifiedRow], conv: &ConvSource, limit: usize) -> Vec<String> {
        let mut cursor = CompositeCursor::default();
        let mut emitted: Vec<String> = Vec::new();
        for _ in 0..1000 {
            let lane = conv.page(cursor.conv_page_token.as_deref());
            let result = merge_and_paginate(local_window.to_vec(), lane, &cursor, limit);
            emitted.extend(
                result.candidates[..result.emit_count]
                    .iter()
                    .map(|r| r.legacy.session_id.clone()),
            );
            match result.next_cursor {
                Some(c) => cursor = c,
                None => return emitted,
            }
        }
        panic!("pagination did not terminate");
    }

    fn ids(rows: &[UnifiedRow]) -> Vec<String> {
        rows.iter().map(|r| r.legacy.session_id.clone()).collect()
    }

    #[test]
    fn cursor_round_trips() {
        let cur = CompositeCursor {
            boundary: Some(BoundaryKey {
                updated_at: "2026-06-01T00:00:00Z".into(),
                kind: SessionKind::Chat,
                session_id: "conv_1".into(),
            }),
            conv_page_token: Some("p3".into()),
            conv_page_drained: true,
        };
        let decoded = CompositeCursor::decode(Some(&cur.encode()));
        assert_eq!(decoded.conv_page_token.as_deref(), Some("p3"));
        assert!(decoded.conv_page_drained);
        let b = decoded.boundary.unwrap();
        assert_eq!(b.session_id, "conv_1");
        assert_eq!(b.kind, SessionKind::Chat);
    }

    #[test]
    fn malformed_cursor_decodes_to_fresh_first_page() {
        for bad in [Some("not base64 !!!"), Some(""), None] {
            let c = CompositeCursor::decode(bad);
            assert!(c.boundary.is_none());
            assert!(c.conv_page_token.is_none());
            assert!(!c.conv_page_drained);
        }
    }

    #[test]
    fn multi_page_walk_equals_single_fetch_window() {
        let local_window = vec![
            local("l1", "2026-06-10T00:00:00Z"),
            local("l2", "2026-06-08T00:00:00Z"),
            local("l3", "2026-06-04T00:00:00Z"),
            local("l4", "2026-05-30T00:00:00Z"),
        ];
        let conv_rows = vec![
            conv("c1", "2026-06-09T00:00:00Z"),
            conv("c2", "2026-06-07T00:00:00Z"),
            conv("c3", "2026-06-06T00:00:00Z"),
            conv("c4", "2026-06-03T00:00:00Z"),
            conv("c5", "2026-05-29T00:00:00Z"),
        ];

        let mut expected_all = local_window.clone();
        expected_all.extend(conv_rows.clone());
        expected_all.sort_by(cmp_total_order);
        let expected_ids = ids(&expected_all);

        for &limit in &[1usize, 2, 3, 5, 7, 100] {
            for &page_size in &[1usize, 2, 3] {
                let source = ConvSource::new(conv_rows.clone(), page_size);
                let got = walk_all(&local_window, &source, limit);
                let unique: BTreeSet<&String> = got.iter().collect();
                assert_eq!(
                    unique.len(),
                    got.len(),
                    "duplicate emitted (limit={limit}, page_size={page_size}): {got:?}"
                );
                assert_eq!(
                    got, expected_ids,
                    "walk != single fetch (limit={limit}, page_size={page_size})"
                );
            }
        }
    }

    #[test]
    fn equal_updated_at_tie_break_no_drop_or_dup() {
        let ts = "2026-06-01T00:00:00Z";
        let local_window = vec![local("l_same", ts), local("l_old", "2026-05-01T00:00:00Z")];
        let conv_rows = vec![conv("c_same", ts), conv("c_old", "2026-05-15T00:00:00Z")];

        let mut expected_all = local_window.clone();
        expected_all.extend(conv_rows.clone());
        expected_all.sort_by(cmp_total_order);
        let expected_ids = ids(&expected_all);
        assert_eq!(expected_ids[0], "l_same");
        assert_eq!(expected_ids[1], "c_same");

        for &limit in &[1usize, 2, 3] {
            let source = ConvSource::new(conv_rows.clone(), 1);
            let got = walk_all(&local_window, &source, limit);
            let unique: BTreeSet<&String> = got.iter().collect();
            assert_eq!(unique.len(), got.len(), "dup at limit={limit}: {got:?}");
            assert_eq!(
                got, expected_ids,
                "tie-break walk mismatch at limit={limit}"
            );
        }
    }

    #[test]
    fn partial_conv_page_is_not_advanced_until_drained() {
        let local_window = vec![
            local("l1", "2026-06-10T00:00:00Z"),
            local("l2", "2026-06-08T00:00:00Z"),
        ];
        let conv_rows = vec![
            conv("c1", "2026-06-09T00:00:00Z"),
            conv("c2", "2026-06-07T00:00:00Z"),
        ];
        let source = ConvSource::new(conv_rows.clone(), 2);
        let got = walk_all(&local_window, &source, 1);

        let mut expected_all = local_window.clone();
        expected_all.extend(conv_rows.clone());
        expected_all.sort_by(cmp_total_order);
        assert_eq!(got, ids(&expected_all));
    }

    #[test]
    fn whole_page_filtered_out_does_not_drop_later_match() {
        let local_window = vec![
            local("l1", "2026-06-10T00:00:00Z"),
            local("l2", "2026-06-01T00:00:00Z"),
        ];
        let raw = vec![
            conv("c1_drop", "2026-06-09T00:00:00Z"),
            conv("c2_drop", "2026-06-05T00:00:00Z"),
            conv("c3_ok", "2026-06-03T00:00:00Z"),
        ];
        let source = ConvSource::new(raw, 1);

        let mut cursor = CompositeCursor::default();
        let mut emitted: Vec<String> = Vec::new();
        for _ in 0..1000 {
            let lane = match source.page(cursor.conv_page_token.as_deref()) {
                ConvLane::Page {
                    rows,
                    next_token,
                    frontier,
                } => ConvLane::Page {
                    rows: rows
                        .into_iter()
                        .filter(|r| r.legacy.session_id.contains("ok"))
                        .collect(),
                    next_token,
                    frontier,
                },
                other => other,
            };
            let result = merge_and_paginate(local_window.clone(), lane, &cursor, 2);
            emitted.extend(
                result.candidates[..result.emit_count]
                    .iter()
                    .map(|r| r.legacy.session_id.clone()),
            );
            match result.next_cursor {
                Some(c) => cursor = c,
                None => break,
            }
        }

        let mut expected = local_window.clone();
        expected.push(conv("c3_ok", "2026-06-03T00:00:00Z"));
        expected.sort_by(cmp_total_order);
        assert_eq!(emitted, ids(&expected));
        assert!(
            emitted.iter().any(|id| id == "c3_ok"),
            "the later matching conversation must not be dropped"
        );
    }

    #[test]
    fn local_only_when_conversations_skipped() {
        let local_window = vec![
            local("l1", "2026-06-10T00:00:00Z"),
            local("l2", "2026-06-08T00:00:00Z"),
        ];
        let result = merge_and_paginate(
            local_window.clone(),
            ConvLane::Skipped,
            &CompositeCursor::default(),
            10,
        );
        assert_eq!(result.emit_count, 2);
        assert!(result.partial.is_none());
        assert!(result.next_cursor.is_none());
    }

    #[test]
    fn degraded_lane_sets_partial_and_returns_local() {
        let local_window = vec![local("l1", "2026-06-10T00:00:00Z")];
        let result = merge_and_paginate(
            local_window,
            ConvLane::Degraded(PartialReason::Timeout),
            &CompositeCursor::default(),
            10,
        );
        assert_eq!(result.partial, Some(PartialReason::Timeout));
        assert_eq!(result.emit_count, 1);
    }

    #[test]
    fn degraded_mid_walk_with_progress_keeps_live_conv_token() {
        let cursor = CompositeCursor {
            boundary: Some(BoundaryKey {
                updated_at: "2026-06-15T00:00:00Z".into(),
                kind: SessionKind::Build,
                session_id: "z_newer".into(),
            }),
            conv_page_token: Some("p2".into()),
            conv_page_drained: true,
        };
        let result = merge_and_paginate(
            vec![local("l1", "2026-06-10T00:00:00Z")],
            ConvLane::Degraded(PartialReason::Timeout),
            &cursor,
            10,
        );
        assert_eq!(result.emit_count, 1, "the local row is emitted (progress)");
        assert_eq!(result.partial, Some(PartialReason::Timeout));
        let next = result
            .next_cursor
            .expect("progress + live conv token must keep the continuation");
        assert_eq!(next.conv_page_token.as_deref(), Some("p2"));
        assert_eq!(
            next.boundary.as_ref().map(|b| b.session_id.as_str()),
            Some("l1")
        );
    }

    #[test]
    fn degraded_mid_walk_with_no_progress_terminates() {
        let cursor = CompositeCursor {
            boundary: Some(BoundaryKey {
                updated_at: "2026-06-10T00:00:00Z".into(),
                kind: SessionKind::Build,
                session_id: "l1".into(),
            }),
            conv_page_token: Some("p2".into()),
            conv_page_drained: true,
        };
        let result = merge_and_paginate(
            vec![local("l1", "2026-06-10T00:00:00Z")],
            ConvLane::Degraded(PartialReason::Timeout),
            &cursor,
            10,
        );
        assert_eq!(
            result.emit_count, 0,
            "local lane is exhausted (no progress)"
        );
        assert_eq!(result.partial, Some(PartialReason::Timeout));
        assert!(
            result.next_cursor.is_none(),
            "a zero-progress degraded page must terminate, not re-emit an identical cursor"
        );
    }

    #[test]
    fn degraded_first_page_with_no_token_does_not_fabricate_a_cursor() {
        let result = merge_and_paginate(
            Vec::new(),
            ConvLane::Degraded(PartialReason::Error),
            &CompositeCursor::default(),
            10,
        );
        assert_eq!(result.partial, Some(PartialReason::Error));
        assert!(result.next_cursor.is_none());
    }
}
