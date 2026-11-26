//! VisibleLinkMap — per-frame map of clickable link regions on screen.
//!
//! Populated during the scrollback render pass from the `LinkOverlay`
//! (markdown hyperlinks) and citation URLs from web_search / web_fetch
//! tool blocks. Used by the mouse handler for click-to-open.

use ratatui::layout::Rect;

use crate::render::osc8::{LinkOverlay, LinkTarget};

/// A clickable link region on screen.
///
/// A single logical link may span multiple screen rows when word-wrap
/// splits it. Each row segment is a separate `Rect` in `rects`.
#[derive(Debug, Clone)]
pub struct VisibleLink {
    pub rects: Vec<Rect>,
    pub target: LinkTarget,
    pub id: Option<u32>,
}

impl VisibleLink {
    /// Check whether screen position `(col, row)` falls inside any of
    /// this link's row segments.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        self.rects
            .iter()
            .any(|r| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
    }

    /// True when painted cell width equals the URL's display width (bare URL
    /// text on screen, not a short label or wide citation block).
    pub fn looks_like_bare_url_text(&self) -> bool {
        let LinkTarget::Url(url) = &self.target else {
            return false;
        };
        let painted: usize = self.rects.iter().map(|r| usize::from(r.width)).sum();
        painted == unicode_width::UnicodeWidthStr::width(url.as_ref())
    }
}

/// Per-frame map of visible link regions, with generation-based staleness.
#[derive(Debug, Default)]
pub struct VisibleLinkMap {
    links: Vec<VisibleLink>,
    generation: u64,
}

impl VisibleLinkMap {
    /// Find the link at a given screen position, if any.
    pub fn link_at(&self, col: u16, row: u16) -> Option<&VisibleLink> {
        self.links.iter().find(|link| link.contains(col, row))
    }

    /// Whether this map is stale relative to the current scrollback generation.
    pub fn is_stale(&self, current_generation: u64) -> bool {
        self.generation != current_generation
    }

    /// Rebuild the link map from a `LinkOverlay` and citation URLs.
    ///
    /// Consecutive `OverlayLink`s with the same `id` **and** target (a
    /// single link that word-wrapped across rows) are merged into one
    /// `VisibleLink` with multiple `rects`. Same `id` alone is not enough:
    /// markdown ids restart per document, so two visible messages can both
    /// carry `id=0` for different URLs.
    pub fn rebuild(
        &mut self,
        generation: u64,
        overlay: &LinkOverlay,
        citation_links: Vec<VisibleLink>,
    ) {
        self.rebuild_for_context(
            generation,
            overlay,
            citation_links,
            crate::terminal::terminal_context(),
        );
    }

    fn rebuild_for_context(
        &mut self,
        generation: u64,
        overlay: &LinkOverlay,
        citation_links: Vec<VisibleLink>,
        terminal: &crate::terminal::TerminalContext,
    ) {
        self.links.clear();
        self.generation = generation;
        self.links
            .reserve(overlay.links().len() + citation_links.len());
        self.push_overlay_links(overlay, /* merge_from */ 0, terminal);
        self.links
            .extend(citation_links.into_iter().filter_map(|mut link| {
                link.target = crate::render::osc8::resolve_link_target_for_context(
                    &link.target,
                    crate::render::osc8::LinkPresentation::Opaque,
                    terminal,
                )?
                .open_target?;
                Some(link)
            }));
    }

    /// Append overlay links (e.g. `/btw`) without changing generation.
    ///
    /// Same-id+same-target merge applies only *within this append* —
    /// markdown link ids are per-document, so they will not merge with
    /// anything appended earlier this frame (whether that is the scrollback
    /// prefix from [`Self::rebuild`] or a previous
    /// [`Self::append_from_overlay`] call from another overlay source).
    /// Wrapped segments of the same logical link inside `overlay` still
    /// merge correctly.
    ///
    /// Callers that re-append the same source every frame must
    /// [`Self::truncate`] back to the desired prefix length first, otherwise
    /// each frame's links will accumulate.
    pub fn append_from_overlay(&mut self, overlay: &LinkOverlay) {
        let start_len = self.links.len();
        self.push_overlay_links(overlay, start_len, crate::terminal::terminal_context());
    }

    /// Push overlay segments, merging same-id+same-target only with entries
    /// at indices `>= merge_from` (0 for rebuild; map length for append).
    fn push_overlay_links(
        &mut self,
        overlay: &LinkOverlay,
        merge_from: usize,
        terminal: &crate::terminal::TerminalContext,
    ) {
        self.links.reserve(overlay.links().len());
        for link in overlay.links() {
            let Some(target) = crate::render::osc8::resolve_link_target_for_context(
                &link.target,
                link.presentation,
                terminal,
            )
            .and_then(|resolved| resolved.open_target) else {
                continue;
            };
            let width = link.col_end.saturating_sub(link.col_start);
            if width == 0 {
                continue;
            }
            let rect = Rect::new(link.col_start, link.screen_row, width, 1);
            if let Some(id) = link.id
                && self.links.len() > merge_from
                && let Some(prev) = self.links.last_mut()
                && prev.id == Some(id)
                && prev.target == target
            {
                prev.rects.push(rect);
            } else {
                self.links.push(VisibleLink {
                    rects: vec![rect],
                    target,
                    id: link.id,
                });
            }
        }
    }

    /// Truncate to the first `n` links (used to drop previously-appended
    /// overlay links before re-appending for the current frame).
    pub fn truncate(&mut self, n: usize) {
        self.links.truncate(n);
    }

    /// Number of links currently in the map.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    pub fn links(&self) -> &[VisibleLink] {
        &self.links
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::osc8::{LinkOverlay, LinkPresentation, OverlayLink, resolve_link_target};
    use crate::terminal::{TerminalContext, TerminalName};
    use std::sync::Arc;

    fn make_overlay(links: Vec<(u16, u16, u16, &str, Option<u32>)>) -> LinkOverlay {
        let mut overlay = LinkOverlay::new();
        for (row, col_start, col_end, url, id) in links {
            overlay.push(OverlayLink {
                screen_row: row,
                col_start,
                col_end,
                target: LinkTarget::Url(Arc::from(url)),
                presentation: LinkPresentation::Opaque,
                id,
            });
        }
        overlay
    }

    fn link(url: &str, widths: &[u16]) -> VisibleLink {
        VisibleLink {
            rects: widths
                .iter()
                .enumerate()
                .map(|(i, w)| Rect::new(0, i as u16, *w, 1))
                .collect(),
            target: LinkTarget::Url(Arc::from(url)),
            id: None,
        }
    }

    #[test]
    fn looks_like_bare_url_text_when_painted_equals_url_width() {
        let url = "https://example.com";
        let w = unicode_width::UnicodeWidthStr::width(url) as u16;
        assert!(link(url, &[w]).looks_like_bare_url_text());
        assert!(link(url, &[10, w.saturating_sub(10)]).looks_like_bare_url_text());
    }

    #[test]
    fn looks_like_bare_url_text_false_for_short_label() {
        assert!(!link("https://example.com/long/path", &[4]).looks_like_bare_url_text());
    }

    #[test]
    fn looks_like_bare_url_text_false_for_wide_citation_block() {
        let url = "https://example.com";
        let url_w = unicode_width::UnicodeWidthStr::width(url) as u16;
        assert!(!link(url, &[url_w.saturating_add(40)]).looks_like_bare_url_text());
    }

    #[test]
    fn file_target_provenance_survives_overlay_to_visible_map() {
        let path = Arc::<std::path::Path>::from(std::path::Path::new(
            "/tmp/non-display-target/file name.rs",
        ));
        let mut overlay = LinkOverlay::new();
        overlay.push(OverlayLink {
            screen_row: 3,
            col_start: 4,
            col_end: 10,
            target: LinkTarget::File(Arc::clone(&path)),
            presentation: crate::render::osc8::LinkPresentation::Opaque,
            id: None,
        });

        let mut map = VisibleLinkMap::default();
        map.rebuild(1, &overlay, vec![]);

        assert_eq!(map.links()[0].target, LinkTarget::File(Arc::clone(&path)));
        let resolved = resolve_link_target(&map.links()[0].target).expect("resolved file target");
        assert_eq!(resolved.open_target, Some(LinkTarget::File(path)));
        assert_eq!(
            resolved.osc8_url.unwrap().as_ref(),
            "file:///tmp/non-display-target/file%20name.rs"
        );
        assert!(!map.links()[0].looks_like_bare_url_text());
    }

    #[test]
    fn official_vscode_remote_file_is_excluded_from_activation_map() {
        let file = LinkTarget::File(Arc::from(std::path::Path::new("/worktree/src/main.rs")));
        let web = LinkTarget::Url(Arc::from("https://example.com/docs"));
        let mut overlay = LinkOverlay::new();
        for (row, target, presentation) in [
            (3, file, LinkPresentation::SelfResolvingPath),
            (4, web.clone(), LinkPresentation::Opaque),
        ] {
            overlay.push(OverlayLink {
                screen_row: row,
                col_start: 4,
                col_end: 20,
                target,
                presentation,
                id: None,
            });
        }
        let terminal = TerminalContext {
            brand: TerminalName::VsCode,
            is_ssh: true,
            is_official_vscode_remote: true,
            ..Default::default()
        };

        let mut map = VisibleLinkMap::default();
        map.rebuild_for_context(1, &overlay, vec![], &terminal);

        assert_eq!(map.links().len(), 1);
        assert_eq!(map.links()[0].target, web);
        assert!(map.link_at(5, 3).is_none());
        assert!(map.link_at(5, 4).is_some());
    }

    #[test]
    fn cwd_change_stales_map_before_presentation_ownership_flip() {
        let target = LinkTarget::File(Arc::from(std::path::Path::new("/worktree/src/main.rs")));
        let painted = "src/main.rs";
        let mut state = crate::scrollback::ScrollbackState::new();
        let terminal = TerminalContext {
            brand: TerminalName::VsCode,
            is_ssh: true,
            is_official_vscode_remote: true,
            ..Default::default()
        };
        let overlay_for = |cwd: Option<&std::path::Path>| {
            let mut overlay = LinkOverlay::new();
            overlay.push(OverlayLink {
                screen_row: 3,
                col_start: 4,
                col_end: 15,
                target: target.clone(),
                presentation: crate::render::osc8::file_link_presentation(painted, &target, cwd),
                id: None,
            });
            overlay
        };

        state.set_cwd(Some(std::path::PathBuf::from("/other")));
        let mut map = VisibleLinkMap::default();
        map.rebuild_for_context(
            state.generation(),
            &overlay_for(state.cwd()),
            vec![],
            &terminal,
        );
        assert_eq!(map.len(), 1, "opaque relative paint stays EchoAgent-owned");
        assert!(!map.is_stale(state.generation()));

        let new_cwd = std::path::PathBuf::from("/worktree");
        state.set_cwd(Some(new_cwd.clone()));
        assert!(map.is_stale(state.generation()));
        map.rebuild_for_context(
            state.generation(),
            &overlay_for(state.cwd()),
            vec![],
            &terminal,
        );
        assert!(map.is_empty(), "self-resolving paint delegates to VS Code");

        let generation = state.generation();
        state.set_cwd(Some(new_cwd));
        assert_eq!(state.generation(), generation);
        assert!(!map.is_stale(state.generation()));
    }

    #[test]
    fn link_at_hit_and_miss() {
        let mut map = VisibleLinkMap::default();
        let overlay = make_overlay(vec![(5, 10, 20, "https://example.com", Some(1))]);
        map.rebuild(1, &overlay, vec![]);

        assert_eq!(map.links().len(), 1);
        // Hit inside the link
        let hit = map.link_at(15, 5);
        assert!(hit.is_some());
        assert_eq!(
            &*resolve_link_target(&hit.unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://example.com"
        );
        // Miss: wrong row
        assert!(map.link_at(15, 6).is_none());
        // Miss: before start col
        assert!(map.link_at(9, 5).is_none());
        // Miss: at end col (exclusive)
        assert!(map.link_at(20, 5).is_none());
        // Hit: exact start col
        assert!(map.link_at(10, 5).is_some());
        // Hit: last valid col
        assert!(map.link_at(19, 5).is_some());
    }

    #[test]
    fn staleness_tracking() {
        let mut map = VisibleLinkMap::default();
        assert!(map.is_stale(1));

        let overlay = make_overlay(vec![]);
        map.rebuild(1, &overlay, vec![]);
        assert!(!map.is_stale(1));
        assert!(map.is_stale(2));
    }

    #[test]
    fn rebuild_clears_previous_links() {
        let mut map = VisibleLinkMap::default();
        let overlay1 = make_overlay(vec![(0, 0, 5, "https://first.com", None)]);
        map.rebuild(1, &overlay1, vec![]);
        assert_eq!(map.links().len(), 1);

        let overlay2 = make_overlay(vec![
            (1, 0, 3, "https://second.com", None),
            (2, 0, 4, "https://third.com", None),
        ]);
        map.rebuild(2, &overlay2, vec![]);
        assert_eq!(map.links().len(), 2);
        assert_eq!(
            &*resolve_link_target(&map.links()[0].target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://second.com"
        );
    }

    #[test]
    fn zero_width_links_are_skipped() {
        let mut map = VisibleLinkMap::default();
        let overlay = make_overlay(vec![
            (0, 5, 5, "https://zero-width.com", None), // col_start == col_end
            (0, 5, 10, "https://valid.com", None),
        ]);
        map.rebuild(1, &overlay, vec![]);
        assert_eq!(map.links().len(), 1);
        assert_eq!(
            &*resolve_link_target(&map.links()[0].target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://valid.com"
        );
    }

    #[test]
    fn citation_links_are_included() {
        let mut map = VisibleLinkMap::default();
        let overlay = make_overlay(vec![(0, 0, 5, "https://md-link.com", Some(1))]);
        let citations = vec![VisibleLink {
            rects: vec![Rect::new(2, 10, 30, 1)],
            target: LinkTarget::Url(Arc::from("https://citation.com")),
            id: None,
        }];
        map.rebuild(1, &overlay, citations);
        assert_eq!(map.links().len(), 2);

        // Markdown link
        let hit = map.link_at(3, 0);
        assert!(hit.is_some());
        assert_eq!(
            &*resolve_link_target(&hit.unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://md-link.com"
        );

        // Citation link
        let hit = map.link_at(15, 10);
        assert!(hit.is_some());
        assert_eq!(
            &*resolve_link_target(&hit.unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://citation.com"
        );
    }

    #[test]
    fn multiple_links_first_match_wins() {
        let mut map = VisibleLinkMap::default();
        // Two links that overlap on screen (shouldn't happen in practice, but tests precedence)
        let overlay = make_overlay(vec![
            (5, 0, 10, "https://first.com", None),
            (5, 5, 15, "https://second.com", None),
        ]);
        map.rebuild(1, &overlay, vec![]);

        // Position 5 is in both links; first match wins (iter order)
        let hit = map.link_at(5, 5);
        assert!(hit.is_some());
        assert_eq!(
            &*resolve_link_target(&hit.unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://first.com"
        );
    }

    #[test]
    fn empty_overlay_and_no_citations() {
        let mut map = VisibleLinkMap::default();
        let overlay = make_overlay(vec![]);
        map.rebuild(1, &overlay, vec![]);
        assert!(map.is_empty());
        assert!(map.link_at(0, 0).is_none());
    }

    #[test]
    fn wrapped_link_merges_into_single_entry() {
        let mut map = VisibleLinkMap::default();
        // Same id=42 on two consecutive rows (word-wrap)
        let overlay = make_overlay(vec![
            (3, 10, 30, "https://wrapped.com", Some(42)),
            (4, 0, 15, "https://wrapped.com", Some(42)),
        ]);
        map.rebuild(1, &overlay, vec![]);

        // Should be 1 logical link with 2 rects
        assert_eq!(map.links().len(), 1);
        assert_eq!(map.links()[0].rects.len(), 2);
        assert_eq!(
            &*resolve_link_target(&map.links()[0].target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://wrapped.com"
        );

        // Hit on first row segment
        assert!(map.link_at(15, 3).is_some());
        // Hit on second row segment
        assert!(map.link_at(5, 4).is_some());
        // Miss between segments (wrong col on row 4)
        assert!(map.link_at(20, 4).is_none());
    }

    #[test]
    fn different_ids_stay_separate() {
        let mut map = VisibleLinkMap::default();
        let overlay = make_overlay(vec![
            (3, 0, 10, "https://a.com", Some(1)),
            (4, 0, 10, "https://b.com", Some(2)),
        ]);
        map.rebuild(1, &overlay, vec![]);

        assert_eq!(map.links().len(), 2);
    }

    #[test]
    fn same_id_different_url_stays_separate() {
        let mut map = VisibleLinkMap::default();
        let overlay = make_overlay(vec![
            (3, 0, 10, "https://first.com", Some(0)),
            (4, 0, 10, "https://second.com", Some(0)),
            (5, 0, 10, "https://third.com", Some(0)),
        ]);
        map.rebuild(1, &overlay, vec![]);

        assert_eq!(map.links().len(), 3);
        assert_eq!(
            &*resolve_link_target(&map.link_at(5, 3).unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://first.com"
        );
        assert_eq!(
            &*resolve_link_target(&map.link_at(5, 4).unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://second.com"
        );
        assert_eq!(
            &*resolve_link_target(&map.link_at(5, 5).unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://third.com"
        );
        assert_eq!(map.links()[0].rects.len(), 1);
        assert_eq!(map.links()[1].rects.len(), 1);
        assert_eq!(map.links()[2].rects.len(), 1);
    }

    #[test]
    fn none_id_links_never_merge() {
        let mut map = VisibleLinkMap::default();
        let overlay = make_overlay(vec![
            (3, 0, 10, "https://same.com", None),
            (4, 0, 10, "https://same.com", None),
        ]);
        map.rebuild(1, &overlay, vec![]);

        assert_eq!(map.links().len(), 2);
    }

    #[test]
    fn append_does_not_merge_ids_with_scrollback_prefix() {
        let mut map = VisibleLinkMap::default();
        let scrollback = make_overlay(vec![(0, 0, 10, "https://scrollback.com", Some(0))]);
        map.rebuild(1, &scrollback, vec![]);
        assert_eq!(map.len(), 1);

        let btw = make_overlay(vec![(5, 0, 10, "https://btw.com", Some(0))]);
        map.append_from_overlay(&btw);
        assert_eq!(
            map.len(),
            2,
            "colliding per-doc ids must not merge across append"
        );
        assert_eq!(
            &*resolve_link_target(&map.link_at(5, 0).unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://scrollback.com"
        );
        assert_eq!(
            &*resolve_link_target(&map.link_at(5, 5).unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://btw.com"
        );
    }

    #[test]
    fn append_merges_wrapped_segments_within_batch() {
        let mut map = VisibleLinkMap::default();
        map.rebuild(1, &make_overlay(vec![]), vec![]);
        let btw = make_overlay(vec![
            (3, 10, 30, "https://wrapped.com", Some(7)),
            (4, 0, 15, "https://wrapped.com", Some(7)),
        ]);
        map.append_from_overlay(&btw);
        assert_eq!(map.len(), 1);
        assert_eq!(map.links()[0].rects.len(), 2);
        assert!(map.link_at(12, 3).is_some());
        assert!(map.link_at(5, 4).is_some());
    }

    #[test]
    fn truncate_then_append_replaces_overlay_suffix() {
        let mut map = VisibleLinkMap::default();
        let scrollback = make_overlay(vec![(0, 0, 5, "https://sb.com", None)]);
        map.rebuild(1, &scrollback, vec![]);
        let prefix = map.len();
        map.append_from_overlay(&make_overlay(vec![(1, 0, 5, "https://old-btw.com", None)]));
        assert_eq!(map.len(), 2);

        map.truncate(prefix);
        assert_eq!(map.len(), 1);
        map.append_from_overlay(&make_overlay(vec![(2, 0, 5, "https://new-btw.com", None)]));
        assert_eq!(map.len(), 2);
        assert!(map.link_at(1, 1).is_none());
        assert_eq!(
            &*resolve_link_target(&map.link_at(1, 2).unwrap().target)
                .unwrap()
                .osc8_url
                .unwrap(),
            "https://new-btw.com"
        );
    }
}
