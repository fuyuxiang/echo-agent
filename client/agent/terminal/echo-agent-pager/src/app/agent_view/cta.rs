//! Plugin CTA banner and follow-up chips: connect-plugin suggestion
//! rendering/state plus the follow-up chip lifecycle.

#[cfg(test)]
use super::test_fixtures;
use super::{AgentView, CtaPhase, FollowUps, MAX_PENDING_FOLLOW_UPS};
use crate::render::SafeBuf;
use crate::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

impl AgentView {
    /// Refresh the gate for the predicted-next-prompt ghost (tab
    /// autocomplete): it only shows on an idle session's normal prompt.
    /// Called before key dispatch and before each draw so a turn starting
    /// or an input-mode switch hides the ghost immediately. Also re-reads
    /// the enabled state so a `/settings` toggle applies live.
    pub(crate) fn refresh_prompt_suggestion_gate(&mut self) {
        self.prompt.prompt_suggestion.enabled = crate::views::prompt_suggestion::resolve_enabled();
        self.prompt.prompt_suggestion_active = self.prompt_input_mode
            == super::PromptInputMode::Normal
            && matches!(self.prompt_mode, super::PromptMode::Normal)
            // A card's inline editor borrows the composer to answer a question, and a guess at the next prompt to the model is not an
            // answer. In the `/feedback` box it would also paint over the detail placeholder.
            && self.question_view.is_none()
            && !self.session.state.is_busy();
    }

    /// Log the `shown` telemetry impression for the prompt-suggestion ghost
    /// at its *first actual visibility* — exactly once per installed
    /// suggestion (latched in the controller). Visibility is derived per
    /// frame, not fixed at load: a suggestion that arrives behind a
    /// divergent draft (or a closed gate) renders only once the input is
    /// cleared (or the gate re-opens). Called after every gate refresh on
    /// the load path and the prompt key path — the latter *before* the
    /// Tab/Esc intercepts, so `shown` always precedes any `accepted`/
    /// `dismissed` for the same suggestion and the funnel can't exceed 100%.
    pub(crate) fn log_prompt_suggestion_shown_if_visible(&mut self) {
        let Some(ghost) = self.prompt.prompt_suggestion_ghost() else {
            return;
        };
        // Ghost is the remainder after the typed prefix; size the full
        // suggestion (content-free) so shown/accepted measure the same text.
        let full = format!("{}{ghost}", self.prompt.text());
        if !self.prompt.prompt_suggestion.mark_shown_logged() {
            return;
        }
        let (chars, words) = crate::views::prompt_suggestion::suggestion_size(&full);
        echo_agent_telemetry::session_ctx::log_event(
            echo_agent_telemetry::events::PromptSuggestion {
                action: echo_agent_telemetry::events::PromptSuggestionAction::Shown,
                chars,
                words,
            },
        );
    }

    /// Notify the suggestion controller that the prompt text changed.
    /// Returns an Effect to dispatch if the controller wants a debounce.
    ///
    /// Shell suggestions are a bash-mode (`!`) feature: outside it the
    /// pipeline never fires (no shell-history ghosts over natural-language
    /// chat text) and any leftover ghost/dropdown is torn down.
    pub(crate) fn notify_suggestion_text_changed(&mut self) -> Option<super::actions::Effect> {
        use crate::views::suggestion_controller::SuggestionAction;

        if self.prompt_input_mode != super::PromptInputMode::Bash {
            self.prompt.suggestions.clear_ghost();
            return None;
        }

        let snap = self.prompt.slash_state.snapshot();
        let slash_active = snap.active;
        let has_inline_ghost = snap.inline_ghost.is_some();
        // Copy text before passing to text_changed to satisfy the borrow checker.
        let text = self.prompt.text().to_owned();
        let action = self
            .prompt
            .suggestions
            .text_changed(&text, slash_active, has_inline_ghost)?;

        match action {
            SuggestionAction::Matched => None,
            SuggestionAction::Debounce { generation } => {
                Some(super::actions::Effect::DebounceSuggestions {
                    agent_id: self.session.id,
                    generation,
                })
            }
        }
    }

    /// Bump the plugin-CTA debounce generation and request a debounce.
    /// Returns an Effect to dispatch; the match is recomputed when the
    /// debounce expires (see `TaskResult::PluginCtaDebounceExpired`).
    pub(crate) fn notify_plugin_cta_text_changed(&mut self) -> Option<super::actions::Effect> {
        if self.plugin_cta.source_url_or_path.is_none() || self.plugin_cta.candidates.is_empty() {
            return None;
        }
        self.plugin_cta.debounce_generation = self.plugin_cta.debounce_generation.wrapping_add(1);
        Some(super::actions::Effect::DebouncePluginCta {
            agent_id: self.session.id,
            generation: self.plugin_cta.debounce_generation,
        })
    }

    /// Draw the inline plugin CTA into `area` and record click rects.
    /// No-op (clears rects) when the phase is `Hidden` or `area` doesn't fit.
    /// Spinner phases (`Installing`/`AwaitingReload`/`AwaitingMcps`) and the
    /// brief `Installed` confirmation render status text without buttons.
    pub(super) fn draw_plugin_cta(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let tick = self.scrollback.animation_tick();
        let spinner = {
            let frames = crate::glyphs::braille_spinner_frames();
            frames[(tick / crate::views::turn_status::SPINNER_DIVISOR) as usize % frames.len()]
        };
        let secondary = Style::default().fg(theme.text_secondary);
        let name_style = Style::default().fg(theme.accent_model);
        let (left_spans, connect_label): (Vec<Span<'static>>, Option<&'static str>) =
            match &self.plugin_cta.phase {
                CtaPhase::Hidden => {
                    self.plugin_cta.hit_connect.clear();
                    self.plugin_cta.hit_dismiss.clear();
                    return;
                }
                CtaPhase::Matched { name, .. } => (
                    vec![
                        Span::styled("Install ", secondary),
                        Span::styled(name.clone(), name_style),
                        Span::styled(" plugin?", secondary),
                    ],
                    Some("[Install]"),
                ),
                CtaPhase::Installing { name, .. } => (
                    vec![
                        Span::styled(format!("{spinner} Installing "), secondary),
                        Span::styled(name.clone(), name_style),
                        Span::styled(" plugin\u{2026}", secondary),
                    ],
                    None,
                ),
                CtaPhase::AwaitingReload { name } | CtaPhase::AwaitingMcps { name } => (
                    vec![
                        Span::styled(format!("{spinner} Setting up "), secondary),
                        Span::styled(name.clone(), name_style),
                        Span::styled(" plugin\u{2026}", secondary),
                    ],
                    None,
                ),
                CtaPhase::Installed { name } => (
                    vec![
                        Span::styled(name.clone(), name_style),
                        Span::styled(
                            format!(" plugin installed {}", crate::glyphs::check_mark()),
                            secondary,
                        ),
                    ],
                    None,
                ),
                CtaPhase::Error { name, .. } => (
                    vec![
                        Span::styled("Couldn't install ", secondary),
                        Span::styled(name.clone(), name_style),
                        Span::styled(" plugin", secondary),
                    ],
                    Some("[Retry]"),
                ),
            };

        use unicode_width::UnicodeWidthStr;
        let dismiss_label = "[x]";
        const KEY_HINT: &str = " ctrl+/";
        const CTA_HINT_MIN_LEFT: u16 = 12;
        let short_buttons_w = match connect_label {
            Some(label) => label.width() as u16 + 1 + dismiss_label.width() as u16,
            None => 0,
        };
        let key_hint_w = KEY_HINT.width() as u16;
        // Against the label's own width, which the inset shortens by a column.
        let show_hint = connect_label.is_some()
            && area.width.saturating_sub(crate::tips::render::HINT_INSET)
                > short_buttons_w + key_hint_w + CTA_HINT_MIN_LEFT;
        let hint_w = if show_hint { key_hint_w } else { 0 };
        let right_w = short_buttons_w + hint_w;
        if area.height == 0 || area.width <= short_buttons_w {
            self.plugin_cta.hit_connect.clear();
            self.plugin_cta.hit_dismiss.clear();
            return;
        }

        let bg = theme.bg_base;
        for col in 0..area.width {
            if let Some(cell) = buf.cell_mut((area.x + col, area.y)) {
                cell.set_char(' ');
                cell.fg = theme.text_secondary;
                cell.bg = bg;
            }
        }

        let left = Line::from(left_spans);
        // Budget from the inset width, or the label eats the gap before the buttons.
        let text = crate::tips::render::hint_text_area(area);
        let left_budget = if right_w > 0 {
            text.width.saturating_sub(right_w + 1)
        } else {
            text.width
        };
        buf.set_line_safe(text.x, text.y, &left, left_budget);

        let Some(connect_label) = connect_label else {
            self.plugin_cta.hit_connect.clear();
            self.plugin_cta.hit_dismiss.clear();
            return;
        };

        let connect_x = area.x + area.width - right_w;
        let connect_w = connect_label.width() as u16 + hint_w;
        let connect_style = if self.plugin_cta.hit_connect.hovered {
            Style::default().fg(theme.link_fg).bg(theme.bg_hover)
        } else {
            Style::default().fg(theme.text_secondary)
        };
        if show_hint {
            let button = format!("{}{KEY_HINT}]", &connect_label[..connect_label.len() - 1]);
            buf.set_span_safe(
                connect_x,
                area.y,
                &Span::styled(button, connect_style),
                connect_w,
            );
        } else {
            buf.set_span_safe(
                connect_x,
                area.y,
                &Span::styled(connect_label, connect_style),
                connect_w,
            );
        }

        let dismiss_x = connect_x + connect_w + 1;
        let dismiss_style = if self.plugin_cta.hit_dismiss.hovered {
            Style::default().fg(theme.text_secondary).bg(theme.bg_hover)
        } else {
            Style::default().fg(theme.gray)
        };
        buf.set_span_safe(
            dismiss_x,
            area.y,
            &Span::styled(dismiss_label, dismiss_style),
            dismiss_label.width() as u16,
        );

        self.plugin_cta.hit_connect.rect = Some(Rect::new(connect_x, area.y, connect_w, 1));
        self.plugin_cta.hit_dismiss.rect = Some(Rect::new(
            dismiss_x,
            area.y,
            dismiss_label.width() as u16,
            1,
        ));
    }

    /// Apply an `echo.agent/follow_ups` notification, keyed by `response_id`
    /// (newest-response-wins).
    ///
    /// Monotonic accept-the-newer: a never-seen `response_id` is strictly newer
    /// than any previously accepted one, so it supersedes the shown chips; a
    /// re-delivery of an already-accepted (hence older) response is ignored, so
    /// a buffer-replay or duplicate cannot clobber the newest chips on any
    /// turn-boundary path, with no reliance on a clear being wired there and no
    /// eviction window that could let a stale id pass as new. A re-delivery of
    /// the currently-shown response refreshes it in place (no-op when
    /// identical); empty `suggestions` retracts that response's chips. Returns
    /// `true` when the displayed chips changed (a redraw is warranted).
    /// Backward-compatible shim used by tests that don't exercise the turn
    /// identity: equivalent to a follow_ups notification with no stamped
    /// `promptId` (the older-shell / replay path). Production always routes
    /// through [`apply_follow_ups_with_prompt`] from `handle_follow_ups`.
    #[cfg(test)]
    pub(crate) fn apply_follow_ups(
        &mut self,
        response_id: String,
        suggestions: Vec<String>,
    ) -> bool {
        self.apply_follow_ups_with_prompt(response_id, None, suggestions)
    }

    /// `apply_follow_ups` with the turn identity (`prompt_id`) the shell stamps
    /// on each `echo.agent/follow_ups` notification (the same `promptId` it stamps on
    /// every `session/update`). The identity makes viewer-adoption dedup
    /// DETERMINISTIC:
    ///
    /// - A re-delivery of the CURRENTLY-ADOPTED turn's follow-ups (its
    ///   `prompt_id` equals `session.current_prompt_id`) re-renders even when its
    ///   chips were cleared by turn adoption — so chips that were applied then
    ///   cleared reappear instead of being lost until reload.
    /// - A buffer-replayed `echo.agent/follow_ups` for a PRIOR turn's `response_id`
    ///   stays rejected by the seen-ring (its `prompt_id` is not the active one),
    ///   so stale chips are never revived on the new turn.
    ///
    /// `prompt_id == None` (older shells, or a replay path that lacks it) is
    /// treated as "not provably the current turn" → it falls back to the
    /// monotonic newest-wins seen-ring and NEVER revives a cleared prior turn.
    pub(crate) fn apply_follow_ups_with_prompt(
        &mut self,
        response_id: String,
        prompt_id: Option<&str>,
        suggestions: Vec<String>,
    ) -> bool {
        // Re-delivery of the currently-shown response: refresh in place.
        if self
            .follow_ups
            .as_ref()
            .is_some_and(|c| c.response_id == response_id)
        {
            if self
                .follow_ups
                .as_ref()
                .is_some_and(|c| c.suggestions == suggestions)
            {
                return false;
            }
            self.follow_up_chips.clear();
            self.hovered_follow_up_chip = None;
            if suggestions.is_empty() {
                // Empty retraction of the currently-shown chips: drop this id
                // from the seen-ring so a later NON-empty delivery for the SAME
                // response can be re-accepted and re-rendered. Otherwise the id
                // (recorded when first accepted) would make the re-delivery hit
                // the `follow_up_seen` reject below and never display. This only
                // ever affects the currently-shown (newest) id — a genuinely
                // older/superseded id is never the shown one, so it never
                // reaches this branch and stays rejected (newest-wins intact).
                self.follow_up_seen.remove(&response_id);
                self.follow_ups = None;
                self.follow_up_shown_prompt_id = None;
            } else {
                self.follow_ups = Some(FollowUps {
                    response_id,
                    suggestions,
                });
                self.follow_up_shown_prompt_id = prompt_id.map(str::to_owned);
            }
            return true;
        }

        // Does this notification belong to the turn the client has currently
        // adopted? Deterministic when the shell stamped the `promptId`; `false`
        // for older shells / replay paths without one (those rely on the
        // newest-wins seen-ring below and never revive a prior turn).
        let current_prompt_id = self.session.current_prompt_id.as_deref();
        let is_current_turn =
            matches!((prompt_id, current_prompt_id), (Some(pid), Some(cur)) if pid == cur);
        // A stamped `promptId` that names a DIFFERENT turn than the one
        // currently adopted: this is a non-current turn's follow_ups (a PRIOR
        // turn's late first-time arrival, or a not-yet-adopted turn). It must
        // never render — as a re-delivery OR as "newest" — while another turn is
        // active, or its chips would appear over the running turn.
        //
        // Guarded on `current == Some`: a `None` `promptId` (older shells) has
        // no turn identity → newest-wins fallback; and `current == None` (e.g. a
        // just-finished turn whose trailing follow_ups arrive after
        // `current_prompt_id` was cleared) is NOT a mismatch, so those chips
        // still render.
        let names_other_active_turn =
            matches!((prompt_id, current_prompt_id), (Some(pid), Some(cur)) if pid != cur);

        if self.follow_up_seen.contains_key(&response_id) {
            // Already accepted. Normally this is an older, superseded response →
            // reject (newest-wins; a stale prior-turn buffer-replay must NOT
            // revive chips). EXCEPTION: if this IS the currently-adopted turn
            // (its `prompt_id` matches the active turn) and it carries chips, a
            // re-delivery whose chips were cleared by turn adoption must
            // re-render — scoped deterministically to the active turn so a prior
            // turn is never revived.
            if is_current_turn && !suggestions.is_empty() {
                self.follow_up_chips.clear();
                self.hovered_follow_up_chip = None;
                self.follow_ups = Some(FollowUps {
                    response_id,
                    suggestions,
                });
                self.follow_up_shown_prompt_id = prompt_id.map(str::to_owned);
                return true;
            }
            return false;
        }

        // First-time (never-seen) arrival for a turn that is NOT the active one.
        // It must not render NOW (it would draw over the running turn), but it
        // may be a not-yet-adopted FUTURE turn whose follow_ups raced ahead of
        // the `session/update` that adopts it. Dropping it would lose the chips
        // forever if it is the only delivery. Instead BUFFER it keyed by its
        // `promptId`; [`flush_pending_follow_ups`] renders it if/when that turn
        // becomes current. A genuinely prior turn's `promptId` never becomes
        // current again, so its buffered entry is never flushed (no stale
        // revival) and is eventually FIFO-evicted by the cap.
        if names_other_active_turn {
            if let Some(pid) = prompt_id
                && !suggestions.is_empty()
            {
                self.buffer_pending_follow_ups(pid.to_owned(), response_id, suggestions);
            }
            return false;
        }

        // Strictly newer response: supersede the prior chips (already recorded
        // in `follow_up_seen` at its own acceptance, so no re-record needed).
        let had_chips = self.follow_ups.take().is_some();
        self.follow_up_shown_prompt_id = None;
        self.follow_up_chips.clear();
        self.hovered_follow_up_chip = None;
        if suggestions.is_empty() {
            // An empty payload for a never-seen response is a no-op retraction
            // and is deliberately NOT recorded, so a later non-empty delivery
            // for the same response still renders.
            return had_chips;
        }
        self.follow_up_seen
            .insert(response_id.clone(), self.follow_up_next_gen);
        self.follow_up_next_gen += 1;
        self.follow_ups = Some(FollowUps {
            response_id,
            suggestions,
        });
        self.follow_up_shown_prompt_id = prompt_id.map(str::to_owned);
        true
    }

    /// Buffer a stamped `echo.agent/follow_ups` for a turn that is not yet current,
    /// keyed by its `promptId`. A newer delivery for the same `promptId`
    /// overwrites the earlier one (keep the latest); the FIFO order list bounds
    /// the map to [`MAX_PENDING_FOLLOW_UPS`], evicting only the oldest entry.
    fn buffer_pending_follow_ups(
        &mut self,
        prompt_id: String,
        response_id: String,
        suggestions: Vec<String>,
    ) {
        let is_new_key = self
            .follow_up_pending
            .insert(
                prompt_id.clone(),
                FollowUps {
                    response_id,
                    suggestions,
                },
            )
            .is_none();
        if is_new_key {
            self.follow_up_pending_order.push_back(prompt_id);
            if self.follow_up_pending_order.len() > MAX_PENDING_FOLLOW_UPS
                && let Some(evicted) = self.follow_up_pending_order.pop_front()
            {
                self.follow_up_pending.remove(&evicted);
            }
        }
    }

    /// Flush a buffered `echo.agent/follow_ups` for `prompt_id` (a turn that has just
    /// become current). Renders the chips through [`apply_follow_ups_with_prompt`]
    /// — now that `current_prompt_id == prompt_id`, the stamped delivery is
    /// accepted as the active turn's. Returns whether chips were rendered. A
    /// no-op when nothing is buffered for `prompt_id`. Callers invoke this AFTER
    /// setting `current_prompt_id` to `prompt_id` at every turn-adoption site.
    pub(crate) fn flush_pending_follow_ups(&mut self, prompt_id: &str) -> bool {
        let Some(pending) = self.follow_up_pending.remove(prompt_id) else {
            return false;
        };
        if let Some(pos) = self
            .follow_up_pending_order
            .iter()
            .position(|p| p == prompt_id)
        {
            self.follow_up_pending_order.remove(pos);
        }
        self.apply_follow_ups_with_prompt(pending.response_id, Some(prompt_id), pending.suggestions)
    }

    /// Drop the shown follow-up chips at a turn start (UX: they belong to the
    /// previous response). The response stays recorded in `follow_up_seen`, so a
    /// stale re-delivery stays rejected; the active turn's own re-delivery still
    /// re-renders via the `prompt_id` match in [`apply_follow_ups_with_prompt`],
    /// so this is used for BOTH viewer-adoption and self-driven turn starts.
    pub(crate) fn clear_follow_ups(&mut self) {
        self.follow_ups = None;
        self.follow_up_shown_prompt_id = None;
        self.follow_up_chips.clear();
        self.hovered_follow_up_chip = None;
    }

    /// Full follow-up reset for a session reload. Unlike [`clear_follow_ups`]
    /// (turn boundary — keeps `follow_up_seen` so a stale re-delivery stays
    /// rejected), a reload starts a fresh streaming session: follow-ups never
    /// persist, so the prior session's seen ids must also be dropped or they
    /// would suppress chips streamed after the reload.
    pub(crate) fn reset_follow_ups_for_reload(&mut self) {
        self.reset_follow_ups_for_reload_preserving(None);
    }

    /// Reload reset that PRESERVES the running turn's follow-ups for
    /// `keep_prompt_id` (the turn the load is about to adopt). On `SessionLoaded`
    /// the running turn's `echo.agent/follow_ups` arrive on the ext channel DURING
    /// `loading_replay`; an unconditional reset would drop them before adoption
    /// could re-render them, so the chips would never appear unless the server
    /// resent them. The running turn's chips live in ONE of two places at reset
    /// time:
    ///
    /// * [`follow_up_pending`](Self::follow_up_pending) — buffered, never
    ///   displayed (the turn was not current when the chips arrived); OR
    /// * [`follow_ups`](Self::follow_ups) — already ON SCREEN, because
    ///   `current_prompt_id` was unset or already equalled the running turn, so
    ///   the delivery took the newest-wins / current-turn render path instead
    ///   of the buffer.
    ///
    /// Both are preserved (the on-screen copy is the live, latest state, so it
    /// wins) by re-buffering the survivor into `follow_up_pending` keyed by
    /// `keep_prompt_id`; [`adopt_running_prompt`](Self::adopt_running_prompt)
    /// then flushes it. All other state — every OTHER turn's buffer, the seen
    /// ring, on-screen chips of any other turn — is still cleared, so a reload
    /// never leaves stale chips behind. `None` is a full reset (the
    /// reconnect-reload finalize path, which has no running turn to adopt).
    pub(crate) fn reset_follow_ups_for_reload_preserving(&mut self, keep_prompt_id: Option<&str>) {
        // Capture the running turn's follow_ups BEFORE wiping state. Prefer the
        // on-screen copy (it rendered, so it is the latest accepted delivery);
        // fall back to the pending buffer.
        let kept = keep_prompt_id.and_then(|keep| {
            let displayed = self
                .follow_up_shown_prompt_id
                .as_deref()
                .filter(|shown| *shown == keep)
                .and_then(|_| self.follow_ups.clone());
            displayed
                .or_else(|| self.follow_up_pending.get(keep).cloned())
                .map(|entry| (keep.to_owned(), entry))
        });

        self.follow_ups = None;
        self.follow_up_shown_prompt_id = None;
        self.follow_up_chips.clear();
        self.hovered_follow_up_chip = None;
        self.follow_up_seen.clear();
        self.follow_up_next_gen = 0;
        self.follow_up_pending.clear();
        self.follow_up_pending_order.clear();
        if let Some((pid, entry)) = kept {
            self.follow_up_pending.insert(pid.clone(), entry);
            self.follow_up_pending_order.push_back(pid);
        }
    }

    /// Index of the follow-up chip under a screen position, if any. Used by
    /// the mouse handler to submit the clicked suggestion as a literal prompt.
    pub(crate) fn follow_up_chip_at(&self, col: u16, row: u16) -> Option<usize> {
        self.follow_up_chips
            .iter()
            .position(|r| r.contains((col, row).into()))
    }

    /// Update hover highlight for follow-up chips. Returns true if the hover
    /// index changed (caller should re-render).
    pub(crate) fn set_hovered_follow_up_chip(&mut self, idx: Option<usize>) -> bool {
        if self.hovered_follow_up_chip == idx {
            return false;
        }
        self.hovered_follow_up_chip = idx;
        true
    }

    /// Install the plugin currently surfaced by the CTA. Transitions the CTA
    /// into `Installing` and queues the install effect. Usable from `Matched`
    /// (Connect) and `Error` (Retry); a no-op otherwise or without a session.
    pub(in crate::app) fn connect_matched_plugin(&mut self) {
        let (plugin_relative_path, name, is_retry) = match &self.plugin_cta.phase {
            CtaPhase::Matched {
                plugin_relative_path,
                name,
            } => (plugin_relative_path.clone(), name.clone(), false),
            CtaPhase::Error {
                plugin_relative_path,
                name,
                ..
            } => (plugin_relative_path.clone(), name.clone(), true),
            _ => return,
        };
        echo_agent_telemetry::session_ctx::log_event(
            echo_agent_telemetry::events::PluginCtaConnectClicked {
                plugin_name: name.clone(),
                is_retry,
            },
        );
        let Some(session_id) = self.session.session_id.clone() else {
            return;
        };
        // No install target: the CTA source vanished from the last catalog
        // scan (reachable via Error/Retry), so an install can't be routed.
        let Some(source_url_or_path) = self.plugin_cta.source_url_or_path.clone() else {
            return;
        };
        // Whether to probe for MCP servers after install. URL-sourced plugins
        // are not cloned at scan time, so their `has_mcp` is always false; treat
        // a remote URL as "may ship MCP" and probe anyway, otherwise the post-
        // install handoff is skipped for exactly the plugins that need it.
        let expects_mcp = self
            .plugin_cta
            .candidates
            .iter()
            .find(|c| c.name == name)
            .is_some_and(|c| c.has_mcp || c.remote_url.is_some());
        self.plugin_cta.phase = CtaPhase::Installing {
            plugin_relative_path: plugin_relative_path.clone(),
            name,
        };
        self.plugin_cta.expects_mcp = expects_mcp;
        self.plugin_cta.mcp_attempt = 0;
        self.plugin_cta.hit_connect.clear();
        self.plugin_cta.hit_dismiss.clear();
        self.pending_effects
            .push(super::actions::Effect::InstallPluginFromCta {
                agent_id: self.session.id,
                session_id,
                source_url_or_path,
                plugin_relative_path,
            });
    }
}

#[cfg(test)]
mod prompt_suggestion_gate_tests {
    use super::test_fixtures::make_agent;

    /// A card's inline editor borrows the composer, so the next-prompt ghost must not paint into it. The `/feedback` box relies on this
    /// for its placeholder; the old `~` mode got it from `PromptInputMode::Feedback`, which no longer exists.
    #[test]
    fn an_open_question_card_closes_the_suggestion_gate() {
        use crate::views::prompt_widget::StashedPrompt;
        use crate::views::question_view::QuestionViewState;
        use echo_agent_tools::implementations::echo_agent_build::ask_user_question::Question;

        let mut agent = make_agent();
        agent.refresh_prompt_suggestion_gate();
        assert!(
            agent.prompt.prompt_suggestion_active,
            "an idle normal composer is the ghost's home"
        );

        agent.question_view = Some(QuestionViewState::new(
            "gate-test".into(),
            vec![Question {
                question: "which one?".into(),
                options: vec![],
                multi_select: Some(false),
                id: None,
            }],
            StashedPrompt::default(),
        ));
        agent.refresh_prompt_suggestion_gate();
        assert!(
            !agent.prompt.prompt_suggestion_active,
            "a card owns the composer, so the ghost stays out of it"
        );
    }
}

#[cfg(test)]
mod plugin_cta_notify_tests {
    use super::test_fixtures::make_agent;

    fn cta_entry(name: &str) -> echo_agent_hooks_plugins_types::MarketplacePluginEntry {
        echo_agent_hooks_plugins_types::MarketplacePluginEntry {
            name: name.into(),
            version: None,
            description: None,
            category: None,
            author: None,
            tags: Vec::new(),
            keywords: Vec::new(),
            domains: Vec::new(),
            homepage: None,
            relative_path: format!("plugins/{name}"),
            skill_count: 0,
            has_hooks: false,
            has_agents: false,
            has_mcp: false,
            install_status: "not_installed".into(),
            installed_version: None,
            components: None,
            remote_url: None,
            remote_ref: None,
            remote_sha: None,
            remote_subdir: None,
        }
    }

    #[test]
    fn notify_skips_debounce_when_no_candidates() {
        let mut agent = make_agent();
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        agent.plugin_cta.candidates.clear();
        assert!(agent.notify_plugin_cta_text_changed().is_none());
        assert_eq!(agent.plugin_cta.debounce_generation, 0);
    }

    #[test]
    fn notify_skips_debounce_when_source_absent() {
        let mut agent = make_agent();
        agent.plugin_cta.source_url_or_path = None;
        agent.plugin_cta.candidates = vec![cta_entry("figma")];
        assert!(agent.notify_plugin_cta_text_changed().is_none());
    }

    #[test]
    fn notify_emits_debounce_when_candidates_present() {
        let mut agent = make_agent();
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        agent.plugin_cta.candidates = vec![cta_entry("figma")];
        let eff = agent.notify_plugin_cta_text_changed();
        assert!(matches!(
            eff,
            Some(crate::app::actions::Effect::DebouncePluginCta { generation: 1, .. })
        ));
        assert_eq!(agent.plugin_cta.debounce_generation, 1);
    }

    #[test]
    fn connect_matched_enters_installing_and_emits_effect() {
        use crate::app::actions::Effect;
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.session.session_id = Some("sess-1".to_string().into());
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        agent.connect_matched_plugin();

        match &agent.plugin_cta.phase {
            CtaPhase::Installing {
                plugin_relative_path,
                name,
            } => {
                assert_eq!(name.as_str(), "figma");
                assert_eq!(plugin_relative_path.as_str(), "plugins/figma");
            }
            other => panic!("expected Installing, got {other:?}"),
        }
        assert_eq!(agent.pending_effects.len(), 1);
        match &agent.pending_effects[0] {
            Effect::InstallPluginFromCta {
                source_url_or_path,
                plugin_relative_path,
                ..
            } => {
                assert_eq!(
                    source_url_or_path,
                    echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL
                );
                assert_eq!(plugin_relative_path.as_str(), "plugins/figma");
            }
            other => panic!("expected InstallPluginFromCta, got {other:?}"),
        }
    }

    #[test]
    fn connect_uses_cta_source_url_when_set() {
        use crate::app::actions::Effect;
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.session.session_id = Some("sess-1".to_string().into());
        agent.plugin_cta.source_url_or_path = Some("/srv/spacex-marketplace".into());
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/starlink".into(),
            name: "starlink".into(),
        };
        agent.connect_matched_plugin();

        assert_eq!(agent.pending_effects.len(), 1);
        match &agent.pending_effects[0] {
            Effect::InstallPluginFromCta {
                source_url_or_path, ..
            } => assert_eq!(source_url_or_path, "/srv/spacex-marketplace"),
            other => panic!("expected InstallPluginFromCta, got {other:?}"),
        }
    }

    #[test]
    fn connect_retries_from_error() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.session.session_id = Some("sess-1".to_string().into());
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        agent.plugin_cta.phase = CtaPhase::Error {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
            message: "boom".into(),
        };
        agent.connect_matched_plugin();

        assert!(matches!(
            agent.plugin_cta.phase,
            CtaPhase::Installing { .. }
        ));
        assert_eq!(agent.pending_effects.len(), 1);
    }

    #[test]
    fn connect_without_session_is_noop() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        agent.connect_matched_plugin();
        assert!(matches!(agent.plugin_cta.phase, CtaPhase::Matched { .. }));
        assert!(agent.pending_effects.is_empty());
    }

    #[test]
    fn connect_without_source_is_noop() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.session.session_id = Some("sess-1".to_string().into());
        // Reachable via Error/Retry after the CTA source vanished from a
        // refresh scan; without an install target the click must do nothing.
        agent.plugin_cta.source_url_or_path = None;
        agent.plugin_cta.phase = CtaPhase::Error {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
            message: "boom".into(),
        };
        agent.connect_matched_plugin();
        assert!(matches!(agent.plugin_cta.phase, CtaPhase::Error { .. }));
        assert!(agent.pending_effects.is_empty());
    }

    #[test]
    fn connect_captures_expects_mcp_and_resets_attempt() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.session.session_id = Some("sess-1".to_string().into());
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        let mut entry = cta_entry("figma");
        entry.has_mcp = true;
        agent.plugin_cta.candidates = vec![entry];
        agent.plugin_cta.mcp_attempt = 7;
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        agent.connect_matched_plugin();
        assert!(agent.plugin_cta.expects_mcp);
        assert_eq!(agent.plugin_cta.mcp_attempt, 0);
    }

    #[test]
    fn connect_expects_mcp_false_for_skills_only_plugin() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.session.session_id = Some("sess-1".to_string().into());
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        // cta_entry defaults has_mcp = false (skills-only).
        agent.plugin_cta.candidates = vec![cta_entry("figma")];
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        agent.connect_matched_plugin();
        assert!(!agent.plugin_cta.expects_mcp);
    }

    #[test]
    fn connect_expects_mcp_true_for_url_sourced_plugin() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.session.session_id = Some("sess-1".to_string().into());
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        // URL-sourced plugins report has_mcp = false at scan time (not cloned
        // yet); a remote URL must still trigger the post-install MCP probe.
        let mut entry = cta_entry("figma");
        entry.has_mcp = false;
        entry.remote_url = Some("https://github.com/acme/figma-plugin.git".into());
        agent.plugin_cta.candidates = vec![entry];
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        agent.connect_matched_plugin();
        assert!(agent.plugin_cta.expects_mcp);
    }

    fn cta_row_text(buf: &ratatui::buffer::Buffer, area: ratatui::layout::Rect) -> String {
        (0..area.width)
            .filter_map(|x| {
                buf.cell((area.x + x, area.y))
                    .map(|c| c.symbol().to_string())
            })
            .collect()
    }

    #[test]
    fn draw_installing_shows_label_spinner_and_no_buttons() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.phase = CtaPhase::Installing {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        let area = ratatui::layout::Rect::new(0, 0, 60, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        agent.draw_plugin_cta(&mut buf, area, &crate::theme::Theme::current());

        let row = cta_row_text(&buf, area);
        assert!(row.contains("Installing figma"), "row = {row:?}");
        // Leading braille spinner (frame 0 at tick 0).
        assert!(
            row.contains(crate::glyphs::braille_spinner_frames()[0]),
            "row = {row:?}"
        );
        assert!(agent.plugin_cta.hit_connect.rect.is_none());
        assert!(agent.plugin_cta.hit_dismiss.rect.is_none());
    }

    #[test]
    fn draw_awaiting_phases_show_setting_up_with_spinner_and_no_buttons() {
        use crate::app::agent_view::CtaPhase;
        for phase in [
            CtaPhase::AwaitingReload {
                name: "figma".into(),
            },
            CtaPhase::AwaitingMcps {
                name: "figma".into(),
            },
        ] {
            let mut agent = make_agent();
            agent.plugin_cta.phase = phase;
            let area = ratatui::layout::Rect::new(0, 0, 60, 1);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            agent.draw_plugin_cta(&mut buf, area, &crate::theme::Theme::current());

            let row = cta_row_text(&buf, area);
            assert!(row.contains("Setting up figma"), "row = {row:?}");
            assert!(
                row.contains(crate::glyphs::braille_spinner_frames()[0]),
                "row = {row:?}"
            );
            assert!(agent.plugin_cta.hit_connect.rect.is_none());
            assert!(agent.plugin_cta.hit_dismiss.rect.is_none());
        }
    }

    #[test]
    fn draw_installed_shows_checkmark_and_no_buttons() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.phase = CtaPhase::Installed {
            name: "figma".into(),
        };
        let area = ratatui::layout::Rect::new(0, 0, 60, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        agent.draw_plugin_cta(&mut buf, area, &crate::theme::Theme::current());

        let row = cta_row_text(&buf, area);
        assert!(row.contains("figma plugin installed"), "row = {row:?}");
        assert!(row.contains(crate::glyphs::check_mark()), "row = {row:?}");
        assert!(agent.plugin_cta.hit_connect.rect.is_none());
        assert!(agent.plugin_cta.hit_dismiss.rect.is_none());
    }

    #[test]
    fn draw_matched_shows_install_copy_and_colored_name() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        let area = ratatui::layout::Rect::new(0, 0, 60, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let theme = crate::theme::Theme::current();
        agent.draw_plugin_cta(&mut buf, area, &theme);

        let row = cta_row_text(&buf, area);
        assert!(row.contains("Install figma plugin?"), "row = {row:?}");
        assert!(row.contains("[Install ctrl+/]"), "row = {row:?}");
        assert!(row.contains("[x]"), "row = {row:?}");
        assert!(agent.plugin_cta.hit_connect.rect.is_some());
        assert!(agent.plugin_cta.hit_dismiss.rect.is_some());
        // The plugin name stands out in the accent_model color.
        let name_colored = (0..area.width).any(|x| {
            buf.cell((x, 0))
                .is_some_and(|c| c.fg == theme.accent_model && c.symbol() != " ")
        });
        assert!(name_colored, "expected the plugin name in accent_model");
        let rect = agent.plugin_cta.hit_connect.rect.unwrap();
        for x in rect.x..rect.x + rect.width {
            let cell = buf.cell((x, 0)).unwrap();
            assert_eq!(cell.fg, theme.text_secondary, "col {x}");
        }
    }

    #[test]
    fn draw_matched_hovered_connect_highlights() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        agent.plugin_cta.hit_connect.hovered = true;
        let area = ratatui::layout::Rect::new(0, 0, 60, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let theme = crate::theme::Theme::current();
        agent.draw_plugin_cta(&mut buf, area, &theme);

        let rect = agent.plugin_cta.hit_connect.rect.unwrap();
        for x in rect.x..rect.x + rect.width {
            let cell = buf.cell((x, 0)).unwrap();
            assert_eq!(cell.fg, theme.link_fg, "col {x}");
            assert_eq!(cell.bg, theme.bg_hover, "col {x}");
        }
    }

    #[test]
    fn draw_error_shows_retry_and_dismiss_rects() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.phase = CtaPhase::Error {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
            message: "boom".into(),
        };
        let area = ratatui::layout::Rect::new(0, 0, 60, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        agent.draw_plugin_cta(&mut buf, area, &crate::theme::Theme::current());

        let row = cta_row_text(&buf, area);
        assert!(row.contains("Couldn't install figma"), "row = {row:?}");
        assert!(row.contains("[Retry ctrl+/]"), "row = {row:?}");
        assert!(row.contains("[x]"), "row = {row:?}");
        assert!(agent.plugin_cta.hit_connect.rect.is_some());
        assert!(agent.plugin_cta.hit_dismiss.rect.is_some());
    }

    #[test]
    fn draw_matched_shows_keyboard_hint() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        let area = ratatui::layout::Rect::new(0, 0, 60, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        agent.draw_plugin_cta(&mut buf, area, &crate::theme::Theme::current());

        let row = cta_row_text(&buf, area);
        assert!(row.contains("[Install ctrl+/]"), "row = {row:?}");
        let rect = agent.plugin_cta.hit_connect.rect.unwrap();
        assert_eq!(rect.width, "[Install ctrl+/]".len() as u16);
    }

    #[test]
    fn draw_matched_drops_hint_when_narrow_but_keeps_buttons() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        let area = ratatui::layout::Rect::new(0, 0, 20, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        agent.draw_plugin_cta(&mut buf, area, &crate::theme::Theme::current());

        let row = cta_row_text(&buf, area);
        assert!(row.contains("[Install]"), "row = {row:?}");
        assert!(row.contains("[x]"), "row = {row:?}");
        assert!(!row.contains("ctrl+/"), "row = {row:?}");
        assert!(agent.plugin_cta.hit_connect.rect.is_some());
        assert!(agent.plugin_cta.hit_dismiss.rect.is_some());
    }

    #[test]
    fn draw_matched_hint_yields_to_message_at_intermediate_width() {
        use crate::app::agent_view::CtaPhase;
        let mut agent = make_agent();
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        let area = ratatui::layout::Rect::new(0, 0, 30, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        agent.draw_plugin_cta(&mut buf, area, &crate::theme::Theme::current());

        let row = cta_row_text(&buf, area);
        assert!(!row.contains("ctrl+/"), "row = {row:?}");
        assert!(row.contains("[Install]"), "row = {row:?}");
        assert!(row.contains("Install figma"), "row = {row:?}");
    }

    #[test]
    fn ctrl_slash_installs_matched_plugin() {
        use crate::app::agent_view::CtaPhase;
        use crate::app::app_view::InputOutcome;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut agent = make_agent();
        agent.session.session_id = Some("sess-1".to_string().into());
        agent.plugin_cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        agent.plugin_cta.candidates = vec![cta_entry("figma")];
        agent.plugin_cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        let registry = crate::actions::ActionRegistry::defaults();
        let ev = Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL));
        let outcome = agent.handle_input(&ev, &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(matches!(
            agent.plugin_cta.phase,
            CtaPhase::Installing { .. }
        ));
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, crate::app::actions::Effect::InstallPluginFromCta { .. }))
        );
    }

    #[test]
    fn ctrl_slash_ignored_when_cta_hidden() {
        use crate::app::agent_view::CtaPhase;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut agent = make_agent();
        assert_eq!(agent.plugin_cta.phase, CtaPhase::Hidden);
        let registry = crate::actions::ActionRegistry::defaults();
        let ev = Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL));
        agent.handle_input(&ev, &registry);
        assert_eq!(agent.plugin_cta.phase, CtaPhase::Hidden);
        assert!(agent.pending_effects.is_empty());
    }
}
