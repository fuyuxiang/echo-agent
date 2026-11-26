//! Transient user feedback: toasts, ephemeral tips, mode-switch banners,
//! terminal-size notes, clipboard-copy feedback, and their tick timers.

use super::{
    ActivePane, AgentView, CLIPBOARD_TOAST_DEBOUNCE_MS, MODE_BANNER_TOTAL_TICKS, PromptInputMode,
};
#[cfg(test)]
use super::{AgentPane, test_fixtures};
use crate::app::actions::Action;
use std::time::Instant;

impl AgentView {
    /// Show a brief toast message (e.g., "Copied!").
    ///
    /// Displayed for ~3 seconds (90 ticks at 30fps). Previous transient toast
    /// is replaced; [`Self::sticky_toast`] is preserved and returns after this
    /// expires or is dismissed.
    pub fn show_toast(&mut self, msg: &str) {
        self.toast = Some((crate::glyphs::sanitize_toast_message(msg).into_owned(), 90));
    }

    /// Show an ephemeral tip in the banner row above the prompt, gated by the
    /// app-level per-session `seen_counts` map (`AppView::tip_seen_counts`).
    /// Returns true when the tip was newly shown (and the per-session count
    /// incremented in place — never persisted to disk).
    ///
    /// No-op while the row cannot paint (an occluding view — permission,
    /// question, modal, subagent takeover, fullscreen viewer, `/gboom`, the
    /// extensions/agents modals, the goal-detail overlay, or an open prompt
    /// dropdown — a short terminal, the unknown size before the first draw, or a
    /// pending re-measure right after a resize event) so counts, TTL, and
    /// telemetry never burn on an invisible tip.
    pub fn show_ephemeral_tip(
        &mut self,
        tip: crate::tips::EphemeralTip,
        seen_counts: &mut std::collections::HashMap<&'static str, u32>,
    ) -> bool {
        if !self.ephemeral_tip_renderable(self.last_terminal_size.1) {
            return false;
        }
        self.ephemeral_tip.show(tip, seen_counts)
    }

    /// Whether the tip row could paint right now (last drawn size). Lets
    /// app-level triggers skip work that the show gate would refuse anyway.
    pub(crate) fn ephemeral_tip_can_render(&self) -> bool {
        self.ephemeral_tip_renderable(self.last_terminal_size.1)
    }

    /// Single definition of the agent-level eligibility for the clipboard-image
    /// tip: the tip row can paint, no image chips are already attached, and the
    /// current model accepts image input.
    pub(crate) fn clipboard_image_tip_eligible(&self) -> bool {
        self.ephemeral_tip_can_render()
            && self.prompt.images.is_empty()
            && self.session.models.current_model_accepts_images()
    }

    /// One-shot undo-tip show signal from the last `PromptWidget::handle_key`,
    /// routed as an action so dispatch can reach `app.tip_seen_counts`. Fires
    /// only on a qualifying wipe (set exclusively on `PromptEvent::Edited`).
    pub(super) fn take_prompt_tip_signal(&mut self) -> Option<Action> {
        // Undo (wipe-to-empty) takes precedence; the wipe and the typed-keyword
        // nudge are mutually exclusive on a single keypress, so one
        // `Option<Action>` suffices. The plan nudge is suppressed when already
        // in plan mode (the optimistic read), while the turn is busy, or in a
        // special prompt input mode (bash/feedback/remember).
        if self.prompt.take_undo_tip_fire() {
            return Some(Action::ShowUndoTip);
        }
        let in_plan = self.plan_mode_pending.unwrap_or(self.plan_mode_active);
        if self.prompt.take_plan_nudge_fire()
            && !in_plan
            && self.session.state.is_idle()
            && self.prompt_input_mode == PromptInputMode::Normal
        {
            return Some(Action::ShowPlanNudge);
        }
        None
    }

    /// Whether the ephemeral tip needs tick / animation this frame.
    /// False when the session announcement banner occludes the tip slot so a
    /// session-long freeze cannot keep the metronome hot.
    /// Ambient tips extend that freeze to EVERY occluder (permission ask,
    /// modal, dropdown): their TTL burns only while the row can paint, so an
    /// occluder pauses rather than expires them off-screen.
    pub(crate) fn ephemeral_tip_needs_tick(&self) -> bool {
        self.ephemeral_tip.is_active()
            && !self.session_banner_active
            && !self.privacy_banner.active
            && (!self.ephemeral_tip.active_is_ambient() || self.ephemeral_tip_can_render())
    }

    /// Advance tip TTL only when the tip is allowed to tick (see
    /// [`Self::ephemeral_tip_needs_tick`]).
    pub(crate) fn tick_ephemeral_tip(&mut self) -> bool {
        // Word-select tip lifecycle: any prompt divergence since the tip was
        // shown (typed, pasted, dropped — every edit path funnels into the
        // prompt text) retires it, and the snapshot drops once the tip is
        // gone for any reason. A visible tip is always ticking (it arms the
        // metronome), so the sweep runs within a frame of the edit.
        if self.ephemeral_tip.current_key() == Some(crate::tips::word_select::WORD_SELECT_TIP_KEY) {
            if self.word_select_tip_prompt_snapshot.as_deref() != Some(self.prompt.text()) {
                self.ephemeral_tip
                    .clear(crate::tips::word_select::WORD_SELECT_TIP_KEY);
                self.word_select_tip_prompt_snapshot = None;
                return true;
            }
        } else if self.word_select_tip_prompt_snapshot.is_some() {
            self.word_select_tip_prompt_snapshot = None;
        }
        if !self.ephemeral_tip_needs_tick() {
            return false;
        }
        self.ephemeral_tip.tick()
    }

    /// Unified visibility for the ephemeral tip row: no occluding view, a
    /// tall-enough screen, and no resize since the height was measured. Shared
    /// by the show gate and the draw path (reserve + paint), so a view opening
    /// over an already-shown tip also stops the row's reservation until it
    /// closes.
    ///
    /// Most occluders leave an edit-contextual tip active with TTL still
    /// burning (tip may repaint on close). The announcement banner (critical
    /// or promo) is the exception for every tip, and AMBIENT tips freeze
    /// under any occluder: paint yields **and** [`Self::tick_ephemeral_tip`]
    /// freezes TTL so a long-lived occluder cannot burn the tip off-screen
    /// or keep `needs_animation` hot.
    ///
    /// An occluder is anything that, later in the same frame, keeps the banner
    /// row from reaching the user. The transient mode-switch banner and the
    /// inline `/btw` panel are deliberately NOT occluders: the banner owns the
    /// slot ~2 s while the tip's TTL ticks, and `/btw` has its own layout slot
    /// above the banner. The session announcement banner IS an occluder
    /// (long-lived; see `session_banner_active`).
    ///
    /// Drift warning: banner-covering views are also enumerated in two sibling
    /// hand-maintained lists — the pre-overlay inline-media clear in `draw` and
    /// the per-frame `frame_occluder_rects` (dropdowns + goal detail). A new
    /// banner-covering view must be added here too.
    pub(super) fn ephemeral_tip_renderable(&self, screen_height: u16) -> bool {
        let occluded = !self.permission_queue.is_empty()
            || self.question_view.is_some()
            || self.active_modal.is_some()
            // Privacy upsell banner owns the slot until acted on — a
            // session-long occluder like the session announcement banner.
            || self.privacy_banner.active
            // Subagent fullscreen takeover: draw early-returns into
            // draw_subagent_fullscreen and never paints the parent banner.
            || self.active_subagent.is_some()
            // Fullscreen viewers render after the banner paints: image/video/
            // block dim the whole region down to the shortcuts row (banner
            // included). line_viewer's overlay stops at turn_status.y when a
            // turn status shows, so it does NOT always cover the banner — kept
            // anyway as a safe over-refusal (the gate cannot know layout
            // heights, and a tip during viewer reading is unwanted regardless).
            || self.line_viewer.is_some()
            || self.image_viewer.is_some()
            || self.video_viewer.is_some()
            || self.block_viewer.is_some()
            // /gboom dims the same down-to-shortcuts region as the video viewer.
            || self.gboom.is_some()
            // Extensions/agents modals are centered popups (render_modal_window)
            // that capture all input and early-return out of draw; distinct
            // from active_modal. persona_detail only renders atop the agents
            // modal. A tip could at most peek beside the modal, so refuse.
            || self.extensions_modal.is_some()
            || self.agents_modal.is_some()
            // Goal-detail is a vertically-centered overlay painted after the
            // tip; its box only reaches the banner row for tall/content-rich
            // goals, but kept unconditional as a safe over-refusal (like the
            // modals and line_viewer) since a tip during goal reading is
            // unwanted regardless.
            || (self.show_goal_detail && self.goal_state.is_some())
            || self.show_workflows
            // Prompt dropdowns (@/slash/completion/history) render in the
            // row directly above the prompt — the banner row — clearing it.
            || self.prompt.any_dropdown_open()
            || self.session_banner_active;
        !self.terminal_size_stale && crate::tips::tip_row_renderable(occluded, screen_height)
    }

    /// Draw-path re-measure: record the size of the rect this view painted
    /// into, invalidating Kitty image IDs when it changed (terminals clear
    /// GPU data on resize), and mark the measurement fresh again.
    ///
    /// Only draw calls this — the rect can be smaller than the terminal
    /// (dashboard overlay header band/popup, dev tracing split), so a
    /// resize event must NOT write an extrapolated size here; it flags
    /// staleness via `note_terminal_resize` instead and the next draw
    /// re-measures.
    pub(crate) fn note_terminal_size(&mut self, size: (u16, u16)) {
        if self.last_terminal_size != (0, 0) && self.last_terminal_size != size {
            self.inline_media_ids.clear();
            self.inline_media_iterm_emitted.clear();
            crate::terminal::overlay::reset_owner();
        }
        self.last_terminal_size = size;
        self.terminal_size_stale = false;
    }

    /// Event-path resize note: the terminal changed size, so the height in
    /// `last_terminal_size` no longer describes what this view can paint —
    /// chrome (dashboard overlay header/popup, dev tracing split) means the
    /// view's rect is not derivable from the event's full-terminal size.
    /// The ephemeral-tip show gate refuses until the next draw re-measures;
    /// resize draws are debounced (`RESIZE_DEBOUNCE`), so that window is a
    /// frame's worth of events, and a refusal burns nothing.
    pub(crate) fn note_terminal_resize(&mut self) {
        self.terminal_size_stale = true;
    }

    /// Set or clear the sticky status banner (process-wide indicators should
    /// use [`Self::set_sticky_toast_recursive`] on every agent view).
    pub fn set_sticky_toast(&mut self, msg: Option<&str>) {
        self.sticky_toast = msg.map(|m| crate::glyphs::sanitize_toast_message(m).into_owned());
    }

    /// Propagate sticky status to this view and every nested subagent view.
    pub fn set_sticky_toast_recursive(&mut self, msg: Option<&str>) {
        self.set_sticky_toast(msg);
        for child in self.subagent_views.values_mut() {
            child.set_sticky_toast_recursive(msg);
        }
    }

    /// Show a toast with an explicit tick duration.
    pub fn show_toast_ticks(&mut self, msg: &str, ticks: u8) {
        self.toast = Some((
            crate::glyphs::sanitize_toast_message(msg).into_owned(),
            ticks,
        ));
    }

    /// Message currently drawn in the toast slot: transient wins while active,
    /// otherwise sticky status (if any).
    pub(super) fn active_toast_message(&self) -> Option<&str> {
        if let Some((ref msg, _)) = self.toast {
            return Some(msg.as_str());
        }
        let sticky = self.sticky_toast.as_deref()?;
        // The mouse-off banner advertises how to re-enable. `Ctrl+R` only works
        // from scrollback, so when the prompt is focused show the
        // `/toggle-mouse-reporting` command instead (it toggles from any pane).
        // Storage keeps the scrollback form; swap the displayed text here.
        if sticky == crate::app::MOUSE_OFF_HINT_SCROLLBACK && self.active_pane == ActivePane::Prompt
        {
            return Some(crate::app::MOUSE_OFF_HINT_PROMPT);
        }
        Some(sticky)
    }

    /// Show a transient "Switched to mode: ..." banner above the prompt.
    ///
    /// Triggered on Shift+Tab mode cycles.
    /// Renders at full visibility for 2 s, then fades out over the final 0.3 s.
    pub fn show_mode_switch_banner(&mut self, mode_name: &str) {
        let msg = format!("Switched to mode: {}", mode_name);
        self.mode_switch_banner = Some((msg, MODE_BANNER_TOTAL_TICKS));
    }

    /// Tick the mode-switch banner timer. Returns true if redraw needed
    /// (active or just expired).
    pub fn tick_mode_banner(&mut self) -> bool {
        if let Some((_, ref mut remaining)) = self.mode_switch_banner {
            if *remaining == 0 {
                self.mode_switch_banner = None;
                return true;
            }
            *remaining = remaining.saturating_sub(1);
            return true; // redraw to advance fade
        }
        false
    }

    /// Copy text to clipboard (a backup file is always written too — see
    /// `copy_text_or_file`) and show the result toast.
    ///
    /// When every trusted clipboard backend fails (common on Apple Terminal
    /// over SSH), the toast points at the backup file
    /// (`~/.echo-agent/last-copy.txt`, or `ECHO_AGENT_COPY_FILE`) instead. The returned
    /// [`CopyDelivery`](crate::clipboard::CopyDelivery) tells callers where
    /// the copy actually landed (clipboard, backup file, or nowhere).
    pub fn copy_to_clipboard(&mut self, text: &str) -> crate::clipboard::CopyDelivery {
        let delivery = crate::clipboard::copy_text_or_file(text);
        self.show_toast_ticks(delivery.toast_message().as_ref(), delivery.toast_ticks());
        delivery
    }

    /// Like [`copy_to_clipboard`] but debounces the toast to prevent
    /// rapid flickering during quick word/line selections.
    pub(super) fn copy_to_clipboard_debounced(&mut self, text: &str) {
        let now = Instant::now();
        let too_soon = self
            .last_clipboard_toast_at
            .is_some_and(|t| now.duration_since(t).as_millis() < CLIPBOARD_TOAST_DEBOUNCE_MS);
        if too_soon {
            // Still deliver (clipboard or file fallback), just skip the toast.
            let _ = crate::clipboard::copy_text_or_file(text);
            return;
        }
        self.last_clipboard_toast_at = Some(now);
        self.copy_to_clipboard(text);
    }

    /// Returns `true` if the terminal can render pixel images. Shows a
    /// toast and returns `false` when no graphics protocol is available.
    pub(crate) fn guard_image_support(&mut self) -> bool {
        if crate::terminal::image::detect_graphics_protocol().supports_images() {
            return true;
        }
        let msg = match crate::terminal::terminal_context().graphics_protocol_skip_reason() {
            Some("tmux") => "Inline images disabled within tmux.",
            _ => "Image rendering not supported in this terminal",
        };
        self.show_toast_ticks(msg, 60);
        false
    }

    /// Tick the transient toast timer. Call once per animation tick.
    /// Returns true if the transient toast was removed (needs redraw so a
    /// sticky banner can reappear).
    pub fn tick_toast(&mut self) -> bool {
        if let Some((_, ref mut remaining)) = self.toast {
            if *remaining == 0 {
                self.toast = None;
                return true;
            }
            *remaining = remaining.saturating_sub(1);
        }
        false
    }

    /// Tick the extensions modal's transient result notice. Returns true if it
    /// just expired (needs a redraw to erase the badge / status line).
    pub fn tick_extensions_result_notice(&mut self) -> bool {
        self.extensions_modal
            .as_mut()
            .is_some_and(|m| m.tick_result_notice())
    }

    /// Open `url` in the system browser. When the opener cannot run (headless
    /// Linux VM, missing `xdg-open`, etc.), push a scrollback system message
    /// with the full URL so the user can copy it, and best-effort copy to the
    /// clipboard (OSC 52 works over SSH even without a local display).
    ///
    /// Unsafe schemes are rejected silently (same as [`open_url_if_safe`]).
    pub(crate) fn open_url_or_show(&mut self, url: &str) {
        use crate::app::link_opener::{OpenUrlResult, browser_unavailable_message, try_open_url};
        use crate::scrollback::block::RenderBlock;
        use crate::terminal::hyperlinks::SchemeFilter;

        match try_open_url(url, SchemeFilter::Standard) {
            OpenUrlResult::Opened | OpenUrlResult::RejectedScheme => {}
            OpenUrlResult::BrowserUnavailable => {
                self.scrollback
                    .push_block(RenderBlock::system(browser_unavailable_message(url)));
                // Best-effort clipboard so SSH/VM users can paste into a
                // browser on another machine without selecting TUI text.
                let _ = crate::clipboard::SystemClipboard::try_set(url);
                self.show_toast("Browser unavailable - URL shown above");
            }
        }
    }

    /// [`Self::open_url_or_show`] minus the automatic clipboard copy, for
    /// server-controlled URLs (MCP elicitation): silently seeding the
    /// clipboard invites pasting attacker-chosen text into a shell on the
    /// headless fallback path. The full URL stays visible in scrollback (and
    /// on the card) for a deliberate manual copy instead.
    pub(crate) fn open_untrusted_url_or_show(&mut self, url: &str) {
        use crate::app::link_opener::{OpenUrlResult, browser_unavailable_message, try_open_url};
        use crate::scrollback::block::RenderBlock;
        use crate::terminal::hyperlinks::SchemeFilter;

        match try_open_url(url, SchemeFilter::Standard) {
            OpenUrlResult::Opened | OpenUrlResult::RejectedScheme => {}
            OpenUrlResult::BrowserUnavailable => {
                self.scrollback
                    .push_block(RenderBlock::system(browser_unavailable_message(url)));
                self.show_toast("Browser unavailable - URL shown above");
            }
        }
    }
}

#[cfg(test)]
mod mouse_off_banner_tests {
    use super::test_fixtures::make_running_agent;
    use super::*;

    #[test]
    fn mouse_off_banner_key_swaps_with_focused_pane() {
        let mut view = make_running_agent();
        view.set_sticky_toast(Some(crate::app::MOUSE_OFF_HINT_SCROLLBACK));

        // Scrollback focus: Ctrl+R works there, so advertise it.
        view.active_pane = AgentPane::Scrollback;
        assert_eq!(
            view.active_toast_message(),
            Some(crate::app::MOUSE_OFF_HINT_SCROLLBACK)
        );

        // Prompt focus: the toggle chord is scrollback-only, so advertise the command.
        view.active_pane = AgentPane::Prompt;
        assert_eq!(
            view.active_toast_message(),
            Some(crate::app::MOUSE_OFF_HINT_PROMPT)
        );

        // A transient toast still wins over the sticky banner, regardless of pane.
        view.show_toast("Copied!");
        assert_eq!(view.active_toast_message(), Some("Copied!"));
    }

    #[test]
    fn non_mouse_sticky_banner_is_not_swapped() {
        let mut view = make_running_agent();
        view.set_sticky_toast(Some("Reconnecting"));
        view.active_pane = AgentPane::Prompt;
        assert_eq!(view.active_toast_message(), Some("Reconnecting"));
    }

    #[test]
    fn show_toast_scrubs_control_chars() {
        let mut view = make_running_agent();
        view.show_toast("a\nb\rc\thttps://echo-agent.invalid");
        let msg = view.toast.as_ref().map(|(m, _)| m.as_str()).unwrap_or("");
        assert!(
            !msg.chars().any(char::is_control),
            "show_toast must scrub controls: {msg:?}"
        );
        assert!(msg.contains("https://echo-agent.invalid"), "{msg:?}");
    }

    #[test]
    fn show_toast_ticks_scrubs_control_chars() {
        let mut view = make_running_agent();
        view.show_toast_ticks("x\ny\tz", 10);
        let msg = view.toast.as_ref().map(|(m, _)| m.as_str()).unwrap_or("");
        assert!(
            !msg.chars().any(char::is_control),
            "show_toast_ticks must scrub controls: {msg:?}"
        );
    }

    #[test]
    fn set_sticky_toast_scrubs_control_chars() {
        let mut view = make_running_agent();
        view.set_sticky_toast(Some("sticky\nline"));
        let msg = view.sticky_toast.as_deref().unwrap_or("");
        assert!(
            !msg.chars().any(char::is_control),
            "sticky toast must scrub controls: {msg:?}"
        );
    }
}
