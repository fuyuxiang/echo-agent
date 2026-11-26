//! Inline media: image/video viewer keys, playback state, media click
//! handling, and mermaid diagram affordances.

use super::{AgentView, InlineVideoState};
use crate::app::app_view::InputOutcome;
use crate::render::SafeBuf;
use crate::terminal::overlay::{self, PostFlush};
use crate::theme::Theme;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// Identity of a media file's contents at load time, keying the failed-load
/// negative cache. On Unix, inode + ctime also catch a same-length in-place
/// rewrite whose mtime is too coarse to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaFileStamp {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime: (i64, i64),
}

impl MediaFileStamp {
    /// `None` when the platform reports no mtime: a `(len, no-mtime)` marker
    /// would match every future rewrite of the same length, so such failures
    /// are never negative-cached (the load is retried instead).
    fn from_metadata(meta: &std::fs::Metadata) -> Option<Self> {
        let modified = meta.modified().ok()?;
        #[cfg(unix)]
        let (ino, ctime) = {
            use std::os::unix::fs::MetadataExt;
            (meta.ino(), (meta.ctime(), meta.ctime_nsec()))
        };
        Some(Self {
            len: meta.len(),
            modified,
            #[cfg(unix)]
            ino,
            #[cfg(unix)]
            ctime,
        })
    }
}

/// Insert `bytes` into the bounded cache, evicting arbitrary entries to fit.
/// An oversized payload (≥ `max_bytes`) is returned to the caller instead of
/// inserted: it is used transiently for the current transmit/place, so one
/// giant image can neither flush the whole cache nor accumulate unbounded
/// bytes across frames.
fn cache_inline_media_bytes(
    cache: &mut std::collections::HashMap<std::path::PathBuf, Vec<u8>>,
    path: &std::path::Path,
    bytes: Vec<u8>,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    if bytes.len() >= max_bytes {
        return Some(bytes);
    }
    let mut total: usize = cache.values().map(Vec::len).sum::<usize>() + bytes.len();
    while total > max_bytes {
        // HashMap iteration order is arbitrary; treat as random eviction.
        let Some(victim) = cache.keys().next().cloned() else {
            break;
        };
        if let Some(evicted) = cache.remove(&victim) {
            total -= evicted.len();
        }
    }
    cache.insert(path.to_path_buf(), bytes);
    None
}

impl AgentView {
    // -- Image viewer input --------------------------------------------------

    /// Handle a key event in the image viewer modal.
    pub(super) fn handle_image_viewer_key(&mut self, key: &KeyEvent) -> InputOutcome {
        use crossterm::event::KeyCode;

        if self.image_viewer.is_none() {
            return InputOutcome::Unchanged;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Clear the Kitty image before closing.
                // Old code bypassed STDERR_OUTPUT_LOCK which could interleave
                // mid-frame. Safe to revert: content is valid escapes, not raw text.
                echo_agent_runtime::util::with_locked_stderr(|stderr| {
                    let clear = PostFlush::from(overlay::clear_kitty());
                    let _ = clear.write_to(stderr);
                });
                self.image_viewer = None;
                self.image_load_rx = None;
                // The viewer's decoded/re-encoded overlay image (tens of MB
                // for screenshots/renders) just dropped; input path, so a
                // synchronous purge lands between interactions.
                crate::memory_release::release_retained_memory("image-viewer-close");
            }
            _ => {}
        }
        InputOutcome::Changed
    }

    // -- Inline media rendering -----------------------------------------------

    /// Build Kitty/iTerm2 escape sequences for an inline media placement.
    pub(super) fn build_inline_media_escapes(
        &mut self,
        placement: &crate::scrollback::render::InlineMediaPlacement,
    ) -> Option<String> {
        use crate::prompt_images::decode_image_dimensions;

        let path = &placement.info.path;

        // During inline video playback, transmit the current frame.
        let is_video_playing = self.inline_video.as_ref().is_some_and(|v| v.path == *path);
        if is_video_playing {
            let vid_id = self.get_or_alloc_media_id(path);
            let video = self.inline_video.as_ref()?;
            let frame_data = &video.frames[video.current_frame];
            let (w, h) = decode_image_dimensions(frame_data)
                .unwrap_or((placement.info.width, placement.info.height));
            let transmit = crate::terminal::image::transmit_inline_image(frame_data, vid_id)?;
            let place = crate::terminal::image::place_inline_image(
                frame_data,
                w,
                h,
                placement.screen_rect,
                placement.full_rows,
                placement.top_crop_rows,
                vid_id,
                true,
            )?;
            return Some(format!("{transmit}{place}"));
        }

        // Static image or video poster frame.
        // Allocate the Kitty id only *after* bytes are in hand: a not-yet-written
        // path (or a failed read) must return `None` without recording an id, or
        // the next time the path is seen `needs_transmit` would be false and only
        // `place` (no `transmit`) would emit — leaving a blank image.
        let needs_transmit = !self.inline_media_ids.contains_key(path);

        // Cache bytes before any id allocation: place-only frames after an
        // eviction (id kept, cache dropped) must reload or they dead-end on
        // a permanent spinner.
        let mut oversized: Option<Vec<u8>> = None;
        if !self.inline_media_cache.contains_key(path) {
            // A missing file keeps polling cheaply (generation may still be
            // writing it) and never reaches the decode/ffmpeg work below.
            let meta = std::fs::metadata(path).ok()?;
            // A failure is retried only when the file changed since: a file
            // caught mid-write self-heals; a genuinely broken one doesn't
            // re-run decode/extraction every frame. No stamp (no mtime) means
            // no change signal: never negative-cache, keep retrying.
            let stamp = MediaFileStamp::from_metadata(&meta);
            if let Some(stamp) = stamp
                && self.inline_media_load_failed.get(path) == Some(&stamp)
            {
                return None;
            }
            let loaded = if placement.info.is_video {
                crate::prompt_images::extract_poster_frame(path).and_then(|(frame_bytes, _, _)| {
                    crate::terminal::image::prepare_overlay_image_bytes(&frame_bytes)
                })
            } else {
                std::fs::read(path)
                    .ok()
                    .and_then(|raw| crate::terminal::image::prepare_overlay_image_bytes(&raw))
            };
            let Some(bytes) = loaded else {
                match stamp {
                    Some(stamp) => {
                        self.inline_media_load_failed.insert(path.clone(), stamp);
                    }
                    None => {
                        self.inline_media_load_failed.remove(path);
                    }
                }
                return None;
            };
            self.inline_media_load_failed.remove(path);
            // Bound the cache: a long image-heavy session must not pin
            // every encoded image for its lifetime. Evicting drops only
            // CPU-side bytes; Kitty placements already transmitted stay
            // valid on the GPU (`inline_media_ids` is kept); an evicted
            // path re-reads from disk if it needs a re-transmit.
            const INLINE_MEDIA_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;
            oversized = cache_inline_media_bytes(
                &mut self.inline_media_cache,
                path,
                bytes,
                INLINE_MEDIA_CACHE_MAX_BYTES,
            );
        }
        // Every failure above returned `None` before this, preserving the
        // no-id-without-bytes invariant.
        let image_id = self.get_or_alloc_media_id(path);
        let image_data: &[u8] = match &oversized {
            Some(bytes) => bytes,
            None => self.inline_media_cache.get(path)?,
        };
        let mut transmit_esc = String::new();
        if needs_transmit {
            transmit_esc = crate::terminal::image::transmit_inline_image(image_data, image_id)?;
        }
        let (w, h) = decode_image_dimensions(image_data)
            .unwrap_or((placement.info.width, placement.info.height));

        // iTerm2 has no place-only escape — re-emit when placement moves.
        let emit_iterm = self
            .inline_media_iterm_emitted
            .get(path)
            .is_none_or(|last| *last != placement.screen_rect);
        let place_esc = crate::terminal::image::place_inline_image(
            image_data,
            w,
            h,
            placement.screen_rect,
            placement.full_rows,
            placement.top_crop_rows,
            image_id,
            emit_iterm,
        )?;
        if emit_iterm
            && crate::terminal::image::detect_graphics_protocol()
                == crate::terminal::image::GraphicsProtocol::ITerm2
        {
            self.inline_media_iterm_emitted
                .insert(path.clone(), placement.screen_rect);
        }

        Some(format!("{transmit_esc}{place_esc}"))
    }

    /// Paint each visible Mermaid affordance row (`◇ mermaid [Open Image]
    /// [Copy Image Path] [Copy Source]`) and register its click hit-rects.
    ///
    /// The leading `◇ mermaid` label is a dim, non-clickable marker. Every button
    /// is always clickable (`[Open]`/`[Copy path]` render lazily on click); a
    /// button whose hit-rect is under the mouse is highlighted, the rest are dim.
    /// A trailing dim `rendering…` hint follows the buttons while an on-click
    /// render for that diagram is in flight. The whole layout (label + button +
    /// hint columns) comes from
    /// [`affordance_row`](crate::scrollback::blocks::mermaid_content::affordance_row)
    /// so the painted labels and the hit-rects can't drift, and each segment is
    /// clipped to `screen_rect.width` (which excludes the timestamp reserve).
    pub(super) fn paint_diagram_affordances(
        &mut self,
        buf: &mut Buffer,
        placements: Vec<crate::scrollback::render::DiagramAffordancePlacement>,
        theme: &Theme,
    ) {
        use crate::scrollback::blocks::mermaid_content::affordance_row;
        use ratatui::style::Modifier;
        use unicode_width::UnicodeWidthStr;

        let (hover_col, hover_row) = self.last_mouse_pos;
        for aff in placements {
            let crate::scrollback::render::DiagramAffordancePlacement {
                screen_rect: rect,
                source,
            } = aff;
            // The transient `rendering…` hint shows only while an on-click render
            // for this diagram is in flight.
            let rendering = self.diagram_is_rendering(&source);
            let row = affordance_row(rendering);
            // A segment is drawn only if it fits wholly within the row width
            // (which already excludes the timestamp reserve), so labels never
            // spill past the content area and hit-rects stay inside the row.
            let fits =
                |col: u16, label: &str| col + UnicodeWidthStr::width(label) as u16 <= rect.width;

            // Leading dim, non-clickable `◇ mermaid` label.
            let (label_col, label_text) = row.label;
            if fits(label_col, label_text) {
                buf.set_string_safe(
                    rect.x.saturating_add(label_col),
                    rect.y,
                    label_text,
                    Style::default().fg(theme.gray_dim),
                );
            }

            // Register the diagram's source once — moved, not cloned (the
            // placement is owned and used only here) — when at least one button
            // fits; every fitting button below indexes into it for click routing.
            let source_idx = if row.buttons.iter().any(|b| fits(b.col, b.label)) {
                let idx = self.inline_media_hits.mermaid_sources.len();
                self.inline_media_hits.mermaid_sources.push(source);
                Some(idx)
            } else {
                None
            };
            for btn in row.buttons {
                if !fits(btn.col, btn.label) {
                    continue;
                }
                let bx = rect.x.saturating_add(btn.col);
                let width = UnicodeWidthStr::width(btn.label) as u16;
                let hit = Rect {
                    x: bx,
                    y: rect.y,
                    width,
                    height: 1,
                };
                // Hovered button is highlighted; idle buttons stay at the normal
                // `gray` (brighter than the dim `◇ mermaid` label) so they remain
                // discoverable at rest.
                let style = if hit.contains((hover_col, hover_row).into()) {
                    Style::default()
                        .fg(theme.text_primary)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default().fg(theme.gray)
                };
                buf.set_string_safe(bx, rect.y, btn.label, style);
                if let Some(idx) = source_idx {
                    self.inline_media_hits
                        .mermaid_buttons
                        .push((hit, btn.kind, idx));
                }
            }

            // Trailing dim `rendering…` hint after the buttons (not clickable).
            if let Some((col, status)) = row.status
                && fits(col, status)
            {
                buf.set_string_safe(
                    rect.x.saturating_add(col),
                    rect.y,
                    status,
                    Style::default().fg(theme.gray_dim),
                );
            }
        }
    }

    /// Whether the diagram with `source` has an on-click render in flight (drives
    /// the affordance row's transient `rendering…` hint).
    fn diagram_is_rendering(&self, source: &str) -> bool {
        self.mermaid_is_rendering(source)
    }

    /// Get or allocate a Kitty image ID for the given media path.
    fn get_or_alloc_media_id(&mut self, path: &std::path::Path) -> u32 {
        if let Some(&id) = self.inline_media_ids.get(path) {
            return id;
        }
        let id = self.next_inline_media_id;
        self.next_inline_media_id += 1;
        self.inline_media_ids.insert(path.to_path_buf(), id);
        id
    }

    /// Drain this agent's inline-media placement tracking and return the
    /// Kitty delete escapes for every image it has placed on the GPU.
    ///
    /// Kitty graphics are independent of the cell grid: they survive
    /// redraws until explicitly deleted, and every regular clear path
    /// lives inside [`AgentView::draw`]. When another view takes over the
    /// frame (e.g. the agent dashboard), those per-frame clears stop
    /// running, so the caller uses this to delete whatever this agent
    /// left on screen. Resetting `inline_media_ids` forces a fresh
    /// transmit when this agent next draws; any active inline playback
    /// is stopped, mirroring the scrolled-off-screen clear path.
    ///
    /// Returns `None` when this agent (and its subagent views) has no
    /// placements.
    pub(crate) fn take_inline_media_clear_escapes(&mut self) -> Option<String> {
        let mut clear_esc = self
            .take_own_inline_media_clear_escapes()
            .unwrap_or_default();
        if let Some(esc) = self.take_subagent_inline_media_clear_escapes() {
            clear_esc.push_str(&esc);
        }
        (!clear_esc.is_empty()).then_some(clear_esc)
    }

    /// This view's own placements only, leaving `subagent_views` untouched.
    /// Used by the fullscreen-subagent takeover in [`AgentView::draw`]: the
    /// parent's images must be deleted, but the child is about to draw and
    /// manages its own placements — draining it too would just force a
    /// re-transmit.
    pub(super) fn take_own_inline_media_clear_escapes(&mut self) -> Option<String> {
        // Also proceed when only playback state remains (`inline_video` Some
        // with no active placements — e.g. frames finished loading after the
        // media scrolled off): the drain must still stop the ticking video,
        // or it keeps holding the animation gate open invisibly and its
        // eventual drop is never purged.
        if !self.inline_media_active
            && self.inline_media_ids.is_empty()
            && self.inline_video.is_none()
        {
            return None;
        }
        self.inline_media_active = false;
        self.stop_inline_playback();
        let mut clear_esc = String::new();
        for &id in self.inline_media_ids.values() {
            clear_esc.push_str(&crate::terminal::image::clear_kitty_image(id));
        }
        self.inline_media_ids.clear();
        self.inline_media_iterm_emitted.clear();
        self.last_placed_ids.clear();
        (!clear_esc.is_empty()).then_some(clear_esc)
    }

    /// Stop inline video playback, dropping the pre-extracted frame set
    /// (~50–300 MB), and request a post-draw purge for it. Returns whether a
    /// video was actually playing — callers on the draw path rely on the
    /// deferred request (never a synchronous purge mid-frame), and image-only
    /// paths (`None` here) must not purge at all.
    pub(super) fn stop_inline_playback(&mut self) -> bool {
        let had_video = self.inline_video.take().is_some();
        if had_video {
            crate::memory_release::request_release_after_draw("inline-video-stop");
        }
        had_video
    }

    /// Install freshly-extracted inline video frames, dropping (and
    /// requesting a post-draw purge for) any previous playback's frame set.
    /// Called from the tick path when the background extraction completes.
    pub(crate) fn replace_inline_video(&mut self, video: crate::app::agent_view::InlineVideoState) {
        if self.inline_video.replace(video).is_some() {
            // Switching videos: the previous frame set just dropped.
            crate::memory_release::request_release_after_draw("inline-video-replace");
        }
    }

    /// Subagent fullscreen views render inline media with their own ids —
    /// drain those (recursively), leaving this view's placements alone.
    pub(super) fn take_subagent_inline_media_clear_escapes(&mut self) -> Option<String> {
        let mut clear_esc = String::new();
        for child in self.subagent_views.values_mut() {
            if let Some(esc) = child.take_inline_media_clear_escapes() {
                clear_esc.push_str(&esc);
            }
        }
        (!clear_esc.is_empty()).then_some(clear_esc)
    }

    /// Refresh [`Self::media_link_paths`] — the absolute paths of media
    /// generated in this transcript — from scrollback, but only when its
    /// generation has changed. The model prints short session-relative paths
    /// (`images/1.jpg`); resolving them against the actual generated files ties
    /// each link to the file its message produced (correct across forks) and
    /// never opens an out-of-session or arbitrary file.
    pub(crate) fn ensure_media_link_paths(&mut self) {
        let generation = self.scrollback.generation();
        if self.media_link_paths_gen == Some(generation) {
            return;
        }
        self.media_link_paths_gen = Some(generation);
        self.media_link_paths.clear();
        self.media_link_paths.extend(
            self.scrollback
                .iter_entries()
                .filter_map(|(_, entry)| entry.block.media_ref_path()),
        );
    }

    /// Open a media file in the OS-native default application (Preview,
    /// default video player, etc.). Shared by the `[Open]` button, the
    /// inline-image click target, and the Enter-key handler.
    pub(crate) fn open_media_natively(&mut self, path: &std::path::Path) -> bool {
        if crate::app::link_opener::open_path(path) {
            self.show_toast("Opening in default app\u{2026}");
            true
        } else {
            self.show_toast("Could not open file");
            false
        }
    }

    /// Start or restart inline video playback. If already playing for this
    /// path, restarts from the beginning. Frames are extracted via ffmpeg in
    /// a background thread so the UI never blocks.
    pub(crate) fn start_inline_video_playback(&mut self, path: &std::path::Path) {
        // If already loaded for this path, just restart.
        if let Some(ref mut video) = self.inline_video
            && video.path == path
        {
            video.current_frame = 0;
            video.finished = false;
            video.last_frame_time = std::time::Instant::now();
            return;
        }
        // Extract frames in a background thread to avoid blocking the UI.
        let path_owned = path.to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        self.video_load_rx = Some(rx);
        self.show_toast("Loading video\u{2026}");
        std::thread::spawn(move || {
            let result =
                crate::prompt_images::VideoViewerState::open_from_path(&path_owned).map(|viewer| {
                    InlineVideoState {
                        path: path_owned,
                        frames: viewer.frames,
                        current_frame: 0,
                        last_frame_time: std::time::Instant::now(),
                        fps: viewer.fps,
                        finished: false,
                    }
                });
            let _ = tx.send(result);
        });
    }

    // -- Inline media click handling -----------------------------------------

    /// Handle a click on inline media buttons. Returns `Some(InputOutcome)` if
    /// the click was consumed, `None` to fall through to normal handling.
    pub(in crate::app) fn handle_inline_media_click(
        &mut self,
        col: u16,
        row: u16,
    ) -> Option<InputOutcome> {
        let pos = ratatui::layout::Position::new(col, row);

        // [Open] button or inline image → open natively. Checked before the
        // play targets so a video's [Open] button opens rather than plays.
        let open_target = self
            .inline_media_hits
            .open_buttons
            .iter()
            .chain(self.inline_media_hits.media_areas.iter())
            .find(|(rect, _)| rect.contains(pos))
            .map(|(_, path)| path.clone());
        if let Some(path) = open_target {
            self.open_media_natively(&path);
            return Some(InputOutcome::Changed);
        }

        // [Play] button or video poster → start/restart inline playback.
        let play_target = self
            .inline_media_hits
            .play_buttons
            .iter()
            .chain(self.inline_media_hits.video_play_areas.iter())
            .find(|(rect, _)| rect.contains(pos))
            .map(|(_, path)| path.clone());
        if let Some(path) = play_target {
            self.start_inline_video_playback(&path);
            return Some(InputOutcome::Changed);
        }

        // [Copy] button → copy image to clipboard (async).
        if let Some((_, path)) = self
            .inline_media_hits
            .copy_image_buttons
            .iter()
            .find(|(rect, _)| rect.contains(pos))
        {
            let path = path.clone();
            std::thread::spawn(move || {
                if let Err(e) = echo_agent_runtime::util::clipboard::set_image_file(&path) {
                    tracing::debug!("copy image failed: {e}");
                }
            });
            self.show_toast("Copied image");
            return Some(InputOutcome::Changed);
        }

        // Click on filepath line → copy path to clipboard.
        if let Some((_, path)) = self
            .inline_media_hits
            .filepath_areas
            .iter()
            .find(|(rect, _)| rect.contains(pos))
        {
            let path_str = path.display().to_string();
            self.copy_to_clipboard(&path_str);
            return Some(InputOutcome::Changed);
        }

        // Mermaid affordance row → render-on-click (Open/Copy path) or copy
        // source. Resolve the kind + source index first so the `mermaid_buttons`
        // borrow ends before the `&mut self` dispatch below.
        let mermaid_hit = self
            .inline_media_hits
            .mermaid_buttons
            .iter()
            .find(|(rect, _, _)| rect.contains(pos))
            .map(|&(_, kind, idx)| (kind, idx));
        if let Some((kind, idx)) = mermaid_hit {
            let source = self
                .inline_media_hits
                .mermaid_sources
                .get(idx)
                .cloned()
                .unwrap_or_default();
            self.on_mermaid_affordance_click(kind, source);
            return Some(InputOutcome::Changed);
        }

        None
    }

    /// Route a Mermaid affordance-row click. `[Copy source]` copies the diagram
    /// source (no render); `[Open]`/`[Copy path]` render it lazily at the live
    /// theme/width and then open the PNG / copy its path. `source` is moved into
    /// the renderer, never cloned. `copy_to_clipboard` owns the copy toast.
    fn on_mermaid_affordance_click(
        &mut self,
        kind: crate::scrollback::blocks::mermaid_content::AffordanceKind,
        source: String,
    ) {
        use crate::scrollback::blocks::mermaid_content::AffordanceKind;
        match kind {
            AffordanceKind::CopySource => {
                if !self.copy_to_clipboard(&source).success() {
                    crate::unified_log::error(
                        "mermaid.copy_source.failed",
                        self.session.session_id.as_ref().map(|s| s.0.as_ref()),
                        Some(serde_json::json!({ "source_len": source.len() })),
                    );
                }
            }
            AffordanceKind::Open | AffordanceKind::CopyPath => {
                let action = if matches!(kind, AffordanceKind::Open) {
                    crate::app::mermaid_worker::MermaidClickAction::Open
                } else {
                    crate::app::mermaid_worker::MermaidClickAction::CopyPath
                };
                self.request_mermaid_render(source, action);
            }
        }
    }

    // -- Video viewer input --------------------------------------------------

    /// Handle a key event in the video viewer modal.
    pub(super) fn handle_video_viewer_key(&mut self, key: &KeyEvent) -> InputOutcome {
        use crossterm::event::KeyCode;

        let Some(ref mut viewer) = self.video_viewer else {
            return InputOutcome::Unchanged;
        };

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Clear the Kitty image before closing.
                echo_agent_runtime::util::with_locked_stderr(|stderr| {
                    let clear = PostFlush::from(overlay::clear_kitty());
                    let _ = clear.write_to(stderr);
                });
                self.video_viewer = None;
                // The viewer's pre-extracted frame set (~50–300 MB for a
                // typical clip) just dropped; return the pages to the OS.
                crate::memory_release::release_retained_memory("video-viewer-close");
            }
            KeyCode::Char(' ') => {
                viewer.toggle_play_pause();
            }
            KeyCode::Right | KeyCode::Char('l') => {
                viewer.seek_forward();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                viewer.seek_backward();
            }
            _ => {}
        }
        InputOutcome::Changed
    }

    // -- /gboom easter egg input ------------------------------------------------

    /// Handle a key event in the `/gboom` game modal.
    pub(super) fn handle_gboom_key(&mut self, key: &KeyEvent) -> InputOutcome {
        let Some(ref mut gboom) = self.gboom else {
            return InputOutcome::Unchanged;
        };
        match gboom.handle_key(key) {
            crate::gboom::GboomKeyOutcome::Close => {
                // Clear the kitty image before closing (same as the video
                // viewer) so no stale frame lingers in the cell grid.
                echo_agent_runtime::util::with_locked_stderr(|stderr| {
                    let clear = PostFlush::from(overlay::clear_kitty());
                    let _ = clear.write_to(stderr);
                });
                self.gboom = None;
            }
            crate::gboom::GboomKeyOutcome::Changed => {}
        }
        InputOutcome::Changed
    }

    /// Handle a key-release in the `/gboom` modal (un-latch movement).
    pub(super) fn handle_gboom_release(&mut self, key: &KeyEvent) -> InputOutcome {
        if let Some(ref mut gboom) = self.gboom {
            gboom.handle_release(key);
        }
        InputOutcome::Changed
    }

    pub(super) fn handle_gboom_mouse(&mut self, mouse: &MouseEvent) -> InputOutcome {
        if let Some(ref mut gboom) = self.gboom {
            gboom.handle_mouse(mouse);
        }
        InputOutcome::Changed
    }
}

#[cfg(test)]
mod tests {
    use crate::memory_release::test_support;

    fn make_agent() -> crate::app::agent_view::AgentView {
        crate::test_util::make_agent_view(None, "/tmp")
    }

    fn stub_inline_video() -> crate::app::agent_view::InlineVideoState {
        crate::app::agent_view::InlineVideoState {
            path: std::path::PathBuf::from("/tmp/clip.mp4"),
            frames: vec![Vec::new()],
            current_frame: 0,
            last_frame_time: std::time::Instant::now(),
            fps: 1.0,
            finished: false,
        }
    }

    /// Closing the video viewer modal drops the pre-extracted frame set —
    /// the purge must fire on close and never on other viewer keys.
    #[test]
    fn video_viewer_close_releases_retained_memory() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        test_support::install_counting_hook();

        let mut agent = make_agent();
        agent.video_viewer = Some(crate::prompt_images::VideoViewerState::test_stub());

        // A non-close key keeps the viewer (and its frames) → no purge.
        let before = test_support::calls();
        agent.handle_video_viewer_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(agent.video_viewer.is_some());
        assert_eq!(
            test_support::calls(),
            before,
            "play/pause drops nothing and must not purge"
        );

        // Esc closes → frames drop → one purge.
        let before = test_support::calls();
        agent.handle_video_viewer_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(agent.video_viewer.is_none());
        assert_eq!(
            test_support::calls(),
            before + 1,
            "closing the viewer must purge after the frame set drops"
        );
    }

    /// Draining inline-media placements requests a POST-DRAW purge only when
    /// live playback (a frame set) was actually dropped — image-only clears
    /// must not, and the purge must never run synchronously (these paths sit
    /// inside `draw`). Serialized: the deferred-request flag is process-wide.
    #[test]
    #[serial_test::serial(MEMORY_RELEASE_DEFER)]
    fn inline_media_clear_defers_release_only_for_video() {
        test_support::install_counting_hook();
        // Drain any stale request left by an earlier test in this group.
        crate::memory_release::run_deferred_release();

        let mut agent = make_agent();

        // Image-only placements active: clear drops no frames → no request.
        agent.inline_media_active = true;
        let before = test_support::calls();
        let _ = agent.take_inline_media_clear_escapes();
        crate::memory_release::run_deferred_release();
        assert_eq!(
            test_support::calls(),
            before,
            "an image-only media clear must not purge"
        );

        // Active inline playback: sync no purge; the drain runs it → one.
        agent.inline_media_active = true;
        agent.inline_video = Some(stub_inline_video());
        let before = test_support::calls();
        let _ = agent.take_inline_media_clear_escapes();
        assert!(agent.inline_video.is_none());
        assert_eq!(
            test_support::calls(),
            before,
            "draw-path video stop must never purge synchronously"
        );
        crate::memory_release::run_deferred_release();
        assert_eq!(
            test_support::calls(),
            before + 1,
            "the post-draw drain must purge the dropped frame set"
        );

        // Orphaned playback (frames finished loading after the media
        // scrolled off: no active flag, no placements): the drain must still
        // stop the video and request its purge.
        agent.inline_media_active = false;
        agent.inline_video = Some(stub_inline_video());
        let before = test_support::calls();
        assert!(agent.take_inline_media_clear_escapes().is_none());
        assert!(
            agent.inline_video.is_none(),
            "orphaned playback must be stopped by the drain"
        );
        crate::memory_release::run_deferred_release();
        assert_eq!(test_support::calls(), before + 1);

        // Nothing at all: the early no-placement return → no request.
        let before = test_support::calls();
        let _ = agent.take_inline_media_clear_escapes();
        crate::memory_release::run_deferred_release();
        assert_eq!(
            test_support::calls(),
            before,
            "a no-op clear must not purge"
        );
    }

    /// Installing freshly-extracted frames purges the PREVIOUS playback's
    /// frame set (deferred), and never purges on first install.
    #[test]
    #[serial_test::serial(MEMORY_RELEASE_DEFER)]
    fn replace_inline_video_defers_release_only_when_replacing() {
        test_support::install_counting_hook();
        crate::memory_release::run_deferred_release();

        let mut agent = make_agent();

        // First install: nothing drops → no request.
        let before = test_support::calls();
        agent.replace_inline_video(stub_inline_video());
        crate::memory_release::run_deferred_release();
        assert_eq!(
            test_support::calls(),
            before,
            "first frame-set install drops nothing and must not purge"
        );

        // Replacement: the old frame set drops → deferred purge.
        let before = test_support::calls();
        agent.replace_inline_video(stub_inline_video());
        assert_eq!(
            test_support::calls(),
            before,
            "tick-path replacement must never purge synchronously"
        );
        crate::memory_release::run_deferred_release();
        assert_eq!(test_support::calls(), before + 1);
    }

    /// Oversized payloads (≥ cap) are handed back for transient use and never
    /// inserted, so repeated oversized loads cannot grow the cache.
    #[test]
    fn oversized_media_bytes_stay_transient_and_never_accumulate() {
        let mut cache = std::collections::HashMap::new();
        let p = |s: &str| std::path::PathBuf::from(s);

        assert!(
            super::cache_inline_media_bytes(&mut cache, &p("/small"), vec![0; 4], 16).is_none()
        );
        assert!(cache.contains_key(&p("/small")));

        let back = super::cache_inline_media_bytes(&mut cache, &p("/big-1"), vec![0; 16], 16);
        assert_eq!(
            back.as_ref().map(Vec::len),
            Some(16),
            "oversized returned transient"
        );
        assert!(!cache.contains_key(&p("/big-1")), "oversized never cached");

        let _ = super::cache_inline_media_bytes(&mut cache, &p("/big-2"), vec![0; 64], 16);
        let _ = super::cache_inline_media_bytes(&mut cache, &p("/big-2"), vec![0; 64], 16);
        assert_eq!(
            cache.len(),
            1,
            "successive oversized loads accumulate nothing"
        );

        // Under-cap inserts still evict down to the bound.
        assert!(
            super::cache_inline_media_bytes(&mut cache, &p("/small-2"), vec![0; 15], 16).is_none()
        );
        assert!(cache.values().map(Vec::len).sum::<usize>() <= 16);
    }

    /// Closing the image viewer drops the decoded overlay image — purge
    /// synchronously (input path), exactly once.
    #[test]
    fn image_viewer_close_releases_retained_memory() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        test_support::install_counting_hook();

        let mut agent = make_agent();
        agent.image_viewer = Some(
            crate::prompt_images::ImageViewerState::open_from_path_deferred(std::path::Path::new(
                "x.png",
            )),
        );
        let before = test_support::calls();
        agent.handle_image_viewer_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(agent.image_viewer.is_none());
        assert_eq!(
            test_support::calls(),
            before + 1,
            "closing the image viewer must purge after the image drops"
        );
    }
}
