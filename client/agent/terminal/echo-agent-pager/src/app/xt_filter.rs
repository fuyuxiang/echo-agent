//! XTVERSION DCS reply filter for the input event channel (parser-integrated
//! model as in helix and similar TUIs). See [`XtversionFilter`].

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use super::event_loop::{TimedInputEvent, is_bare_esc_press};

/// How long the filter stays armed waiting for the reply (opentui uses a
/// non-blocking 5s window); zero-cost after disarm.
const XT_ARM_WINDOW: Duration = Duration::from_secs(5);

/// How long a held partial reply waits for its remaining fragments before
/// being resolved (other terminal UI stacks use 150ms).
pub(super) const XT_FRAGMENT_TIMEOUT: Duration = Duration::from_millis(150);

/// Total bound on one hold, so a terminal trickling valid payload chars
/// cannot stall the event loop beyond this.
pub(super) const XT_MAX_HOLD: Duration = Duration::from_secs(1);

/// Payload size cap; real replies are short (`kitty 0.35.2`).
const XT_MAX_PAYLOAD: usize = 64;

/// Recognizes and swallows the XTVERSION DCS reply arriving through the
/// input event channel. crossterm surfaces `ESC P` as Alt+Shift+P, the payload as
/// plain Char presses, ST as Alt+\ and BEL as Ctrl+G. Events behind a partial
/// prefix are staged so surviving input retains FIFO order and timestamps.
pub(super) struct XtversionFilter {
    armed: bool,
    /// Set on the first `filter()` call, not at construction — a loaded
    /// startup can take seconds before the loop processes its first
    /// batch, and that time must not burn the arm window.
    deadline: Option<Instant>,
    state: XtState,
    staged: Vec<StagedEvent>,
    payload: String,
    completed: Option<String>,
}

enum StagedEvent {
    Tentative(TimedInputEvent),
    PassThrough(TimedInputEvent),
}

#[derive(Clone, Copy, PartialEq)]
enum XtState {
    Idle,
    /// Bare Esc held; a following `P` means a split-read `ESC P`.
    EscHeld,
    AwaitGt,
    AwaitPipe,
    Payload,
    /// Bare Esc inside the payload; a following `\` is a split-read ST.
    PayloadEscHeld,
}

impl XtversionFilter {
    pub(super) fn new() -> Self {
        Self::with_armed(crate::terminal::xtversion::reply_pending())
    }

    fn with_armed(armed: bool) -> Self {
        Self {
            armed,
            deadline: None,
            state: XtState::Idle,
            staged: Vec::new(),
            payload: String::new(),
            completed: None,
        }
    }

    pub(super) fn armed(&self) -> bool {
        self.armed
    }

    pub(super) fn holding(&self) -> bool {
        self.armed
            && self
                .staged
                .iter()
                .any(|event| matches!(event, StagedEvent::Tentative(_)))
    }

    pub(super) fn take_completed(&mut self) -> Option<String> {
        self.completed.take()
    }

    /// Flush held events back (prefix turned out not to be a reply).
    fn flush(&mut self) -> Vec<TimedInputEvent> {
        self.state = XtState::Idle;
        self.payload.clear();
        std::mem::take(&mut self.staged)
            .into_iter()
            .map(|event| match event {
                StagedEvent::Tentative(event) | StagedEvent::PassThrough(event) => event,
            })
            .collect()
    }

    fn release_pass_through(&mut self) -> Vec<TimedInputEvent> {
        std::mem::take(&mut self.staged)
            .into_iter()
            .filter_map(|event| match event {
                StagedEvent::Tentative(_) => None,
                StagedEvent::PassThrough(event) => Some(event),
            })
            .collect()
    }

    fn intro_confirmed(&self) -> bool {
        matches!(self.state, XtState::Payload | XtState::PayloadEscHeld)
    }

    /// Flush pre-intro input; drop confirmed DCS bytes but preserve pass-through.
    pub(super) fn resolve_dead_hold(&mut self) -> Vec<TimedInputEvent> {
        if !self.intro_confirmed() {
            return self.flush();
        }
        tracing::debug!("dropping stalled XTVERSION reply fragment");
        self.state = XtState::Idle;
        self.payload.clear();
        self.release_pass_through()
    }

    /// Remove a complete DCS reply from the batch; pass everything else.
    fn filter(&mut self, events: Vec<TimedInputEvent>) -> Vec<TimedInputEvent> {
        // Don't expire mid-hold once the intro is confirmed: the in-flight
        // reply must resolve (Complete or dead-hold drop), or its tail
        // would pass through as typed text.
        let deadline = *self
            .deadline
            .get_or_insert_with(|| Instant::now() + XT_ARM_WINDOW);
        if self.armed && Instant::now() > deadline && !(self.holding() && self.intro_confirmed()) {
            self.armed = false;
            crate::terminal::xtversion::record_no_reply();
        }
        if !self.armed {
            let mut out = self.resolve_dead_hold();
            out.extend(events);
            return out;
        }

        let mut result = Vec::with_capacity(events.len());
        for ev in events {
            // Once the reply completed mid-batch the filter is done —
            // matching further events would hold them forever.
            if !self.armed {
                result.push(ev);
                continue;
            }
            match self.advance(&ev.event) {
                XtAdvance::Hold => self.staged.push(StagedEvent::Tentative(ev)),
                XtAdvance::PassThrough => {
                    if self.holding() {
                        self.staged.push(StagedEvent::PassThrough(ev));
                    } else {
                        result.push(ev);
                    }
                }
                XtAdvance::Complete => {
                    self.completed = Some(std::mem::take(&mut self.payload));
                    self.state = XtState::Idle;
                    self.armed = false;
                    result.extend(self.release_pass_through());
                }
                // Dead hold: drop a confirmed reply fragment, flush back a
                // pre-intro one; re-evaluate the rejecting event from Idle
                // so a following reply is still caught.
                XtAdvance::Mismatch => {
                    result.append(&mut self.resolve_dead_hold());
                    if matches!(self.advance(&ev.event), XtAdvance::Hold) {
                        self.staged.push(StagedEvent::Tentative(ev));
                    } else {
                        result.push(ev);
                    }
                }
            }
        }
        result
    }

    fn advance(&mut self, ev: &Event) -> XtAdvance {
        use XtState::*;
        if !matches!(ev, Event::Key(_)) {
            return XtAdvance::PassThrough;
        }
        if self.state == Idle && is_dcs_intro(ev) {
            self.state = AwaitGt;
            return XtAdvance::Hold;
        }
        if self.state == Payload && is_dcs_terminator(ev) {
            return XtAdvance::Complete;
        }
        if matches!(self.state, Idle | Payload) && is_bare_esc_press(ev) {
            self.state = if self.state == Idle {
                EscHeld
            } else {
                PayloadEscHeld
            };
            return XtAdvance::Hold;
        }
        let Some(ch) = xt_plain_char(ev) else {
            return XtAdvance::Mismatch;
        };
        match (self.state, ch) {
            (EscHeld, 'P') => self.state = AwaitGt,
            (AwaitGt, '>') => self.state = AwaitPipe,
            (AwaitPipe, '|') => self.state = Payload,
            // Strict alphabet so the first typed char outside a real
            // name+version payload (e.g. a `/slash` command after an
            // unterminated reply) breaks the hold instead of being eaten.
            (Payload, c) if is_xt_payload_char(c) && self.payload.len() < XT_MAX_PAYLOAD => {
                self.payload.push(c)
            }
            (PayloadEscHeld, '\\') => return XtAdvance::Complete,
            _ => return XtAdvance::Mismatch,
        }
        XtAdvance::Hold
    }
}

enum XtAdvance {
    Hold,
    PassThrough,
    Complete,
    Mismatch,
}

/// Apply the filter to a batch and, while a partial reply is held, await
/// follow-up fragments — bounded per-fragment by [`XT_FRAGMENT_TIMEOUT`]
/// and overall by [`XT_MAX_HOLD`] so a trickling terminal can't stall the
/// event loop. Cost: a real bare-Esc press during the arm window is
/// delayed by up to one fragment timeout before flushing back.
pub(super) async fn filter_with_fragment_wait(
    xt_filter: &mut XtversionFilter,
    mut raw_events: Vec<TimedInputEvent>,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TimedInputEvent>,
) -> Vec<TimedInputEvent> {
    raw_events = xt_filter.filter(raw_events);
    let hold_deadline = Instant::now() + XT_MAX_HOLD;
    while xt_filter.holding() {
        if Instant::now() > hold_deadline {
            raw_events.extend(xt_filter.resolve_dead_hold());
            break;
        }
        match tokio::time::timeout(XT_FRAGMENT_TIMEOUT, input_rx.recv()).await {
            Ok(Some(ev)) => {
                let mut more = vec![ev];
                super::event_loop::drain_immediate(&mut more, input_rx);
                raw_events.extend(xt_filter.filter(more));
            }
            _ => {
                raw_events.extend(xt_filter.resolve_dead_hold());
                break;
            }
        }
    }
    if let Some(payload) = xt_filter.take_completed() {
        crate::terminal::xtversion::record_reply(&payload);
        // The startup terminal_context emission raced the async reply —
        // re-emit so the populated xtversion field reaches telemetry.
        if crate::terminal::xtversion::detected().is_some() {
            tokio::task::spawn_blocking(|| {
                let t = crate::terminal::terminal_context().telemetry_snapshot();
                echo_agent_telemetry::session_ctx::log_event(t);
            });
        }
    }
    raw_events
}

/// Real XTVERSION payloads are `name version` strings like
/// `kitty 0.35.2`, `XTerm(388)`, `tmux 3.4`.
fn is_xt_payload_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '(' | ')' | '+')
}

/// ST (`ESC \`) in one read parses as Alt+\; BEL parses as Ctrl+G.
fn is_dcs_terminator(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Key(ke) if ke.kind == KeyEventKind::Press
            && ((ke.code == KeyCode::Char('\\') && ke.modifiers.contains(KeyModifiers::ALT))
                || (ke.code == KeyCode::Char('g') && ke.modifiers == KeyModifiers::CONTROL))
    )
}

/// `ESC P` in one read: crossterm parses it as Alt(+Shift)+P.
fn is_dcs_intro(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Key(ke) if ke.kind == KeyEventKind::Press
            && ke.modifiers.contains(KeyModifiers::ALT)
            && matches!(ke.code, KeyCode::Char('P') | KeyCode::Char('p'))
    )
}

/// Plain payload character (NONE or SHIFT modifiers only).
fn xt_plain_char(ev: &Event) -> Option<char> {
    match ev {
        Event::Key(ke)
            if ke.kind == KeyEventKind::Press
                && (ke.modifiers == KeyModifiers::NONE || ke.modifiers == KeyModifiers::SHIFT) =>
        {
            match ke.code {
                KeyCode::Char(c) => Some(c),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;
    use crossterm::event::{KeyEvent, KeyEventState};

    fn test_instant() -> Instant {
        static NOW: OnceLock<Instant> = OnceLock::new();
        *NOW.get_or_init(Instant::now)
    }

    fn press_mods(code: KeyCode, modifiers: KeyModifiers) -> TimedInputEvent {
        TimedInputEvent {
            event: Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            arrived_at: test_instant(),
        }
    }

    fn press(code: KeyCode) -> TimedInputEvent {
        press_mods(code, KeyModifiers::NONE)
    }

    fn press_shift(code: KeyCode) -> TimedInputEvent {
        press_mods(code, KeyModifiers::SHIFT)
    }

    /// The reply `ESC P > | <payload> ESC \` as crossterm surfaces it in
    /// one read: Alt+Shift+P, plain chars, Alt+\.
    fn dcs_reply_events(payload: &str) -> Vec<TimedInputEvent> {
        let mut evs = vec![press_mods(
            KeyCode::Char('P'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        )];
        evs.push(press(KeyCode::Char('>')));
        evs.push(press(KeyCode::Char('|')));
        for c in payload.chars() {
            if c.is_uppercase() {
                evs.push(press_shift(KeyCode::Char(c)));
            } else {
                evs.push(press(KeyCode::Char(c)));
            }
        }
        evs.push(press_mods(KeyCode::Char('\\'), KeyModifiers::ALT));
        evs
    }

    #[test]
    fn xt_filter_swallows_full_reply() {
        let mut f = XtversionFilter::with_armed(true);
        let out = f.filter(dcs_reply_events("kitty 0.35.2"));
        assert!(out.is_empty());
        assert_eq!(f.take_completed().as_deref(), Some("kitty 0.35.2"));
        assert!(!f.armed());
    }

    #[test]
    fn xt_filter_dead_pre_intro_hold_preserves_fifo_and_timestamps() {
        let start = Instant::now();
        let resize_at = start + Duration::from_millis(4);
        let mut filter = XtversionFilter::with_armed(true);
        let events = vec![
            TimedInputEvent {
                event: press(KeyCode::Esc).event,
                arrived_at: start,
            },
            TimedInputEvent {
                event: Event::Resize(80, 24),
                arrived_at: resize_at,
            },
        ];

        assert!(filter.filter(events).is_empty());
        let output = filter.resolve_dead_hold();

        assert_eq!(output.len(), 2);
        assert_eq!(output[0].event, press(KeyCode::Esc).event);
        assert_eq!(output[0].arrived_at, start);
        assert_eq!(output[1].event, Event::Resize(80, 24));
        assert_eq!(output[1].arrived_at, resize_at);
    }

    #[test]
    fn xt_filter_confirmed_dead_hold_releases_interleaved_pass_through() {
        let start = Instant::now();
        let resize_at = start + Duration::from_millis(5);
        let mut events = dcs_reply_events("x");
        events.pop();
        events.insert(
            4,
            TimedInputEvent {
                event: Event::Resize(80, 24),
                arrived_at: resize_at,
            },
        );
        let mut filter = XtversionFilter::with_armed(true);

        assert!(filter.filter(events).is_empty());
        let output = filter.resolve_dead_hold();

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].event, Event::Resize(80, 24));
        assert_eq!(output[0].arrived_at, resize_at);
    }

    #[test]
    fn xt_filter_confirmed_reply_preserves_interleaved_pass_through_order() {
        let start = Instant::now();
        let mut reply = dcs_reply_events("x");
        let tail = reply.split_off(2);
        let mut events = reply;
        events.push(TimedInputEvent {
            event: Event::Resize(80, 24),
            arrived_at: start,
        });
        events.extend(tail);
        let mut filter = XtversionFilter::with_armed(true);

        let output = filter.filter(events);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].event, Event::Resize(80, 24));
        assert_eq!(output[0].arrived_at, start);
        assert_eq!(filter.take_completed().as_deref(), Some("x"));
    }

    #[test]
    fn xt_filter_passes_surrounding_keys() {
        let mut f = XtversionFilter::with_armed(true);
        let mut evs = vec![press(KeyCode::Char('h')), press(KeyCode::Char('i'))];
        evs.extend(dcs_reply_events("tmux 3.4"));
        evs.push(press(KeyCode::Char('!')));
        let out = f.filter(evs);
        assert_eq!(
            out,
            vec![
                press(KeyCode::Char('h')),
                press(KeyCode::Char('i')),
                press(KeyCode::Char('!')),
            ]
        );
        assert_eq!(f.take_completed().as_deref(), Some("tmux 3.4"));
    }

    #[test]
    fn xt_filter_reply_split_across_batches() {
        let mut f = XtversionFilter::with_armed(true);
        let evs = dcs_reply_events("foot(1.22.0)");
        let (a, b) = evs.split_at(5);
        assert!(f.filter(a.to_vec()).is_empty());
        assert!(f.holding());
        assert!(f.filter(b.to_vec()).is_empty());
        assert_eq!(f.take_completed().as_deref(), Some("foot(1.22.0)"));
    }

    #[test]
    fn xt_filter_flush_returns_held_events() {
        let mut f = XtversionFilter::with_armed(true);
        let prefix = dcs_reply_events("x")[..2].to_vec();
        assert!(f.filter(prefix.clone()).is_empty());
        assert!(f.holding());
        assert_eq!(f.resolve_dead_hold(), prefix);
        assert!(f.take_completed().is_none());
    }

    #[test]
    fn xt_filter_non_reply_keys_flush_partial() {
        let mut f = XtversionFilter::with_armed(true);
        let mut evs = dcs_reply_events("x")[..2].to_vec();
        evs.push(press(KeyCode::Enter));
        let out = f.filter(evs.clone());
        assert_eq!(out, evs);
        assert!(!f.holding());
        assert!(f.take_completed().is_none());
    }

    #[test]
    fn xt_filter_split_esc_intro_and_st() {
        // Split reads surface ESC alone: Esc, P, ... Esc, \.
        let mut f = XtversionFilter::with_armed(true);
        let evs = vec![
            press(KeyCode::Esc),
            press_shift(KeyCode::Char('P')),
            press(KeyCode::Char('>')),
            press(KeyCode::Char('|')),
            press(KeyCode::Char('x')),
            press(KeyCode::Esc),
            press(KeyCode::Char('\\')),
        ];
        assert!(f.filter(evs).is_empty());
        assert_eq!(f.take_completed().as_deref(), Some("x"));
    }

    #[test]
    fn xt_filter_bel_terminator() {
        let mut f = XtversionFilter::with_armed(true);
        let mut evs = dcs_reply_events("st 0.9");
        evs.pop();
        evs.push(press_mods(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(f.filter(evs).is_empty());
        assert_eq!(f.take_completed().as_deref(), Some("st 0.9"));
    }

    #[test]
    fn xt_filter_disarmed_passes_everything() {
        let mut f = XtversionFilter::with_armed(false);
        let evs = dcs_reply_events("kitty 0.35.2");
        assert_eq!(f.filter(evs.clone()), evs);
        assert!(f.take_completed().is_none());
    }

    #[test]
    fn xt_filter_confirmed_fragment_dropped_not_typed() {
        // Unterminated reply followed by typing: fragment is dropped, the
        // typed char survives.
        let mut f = XtversionFilter::with_armed(true);
        let mut evs = dcs_reply_events("x");
        evs.pop();
        evs.push(press(KeyCode::Char('/')));
        let out = f.filter(evs);
        assert_eq!(out, vec![press(KeyCode::Char('/'))]);
        assert!(f.take_completed().is_none());
        assert!(!f.holding());
    }

    #[test]
    fn xt_filter_stray_esc_before_reply_still_caught() {
        let mut f = XtversionFilter::with_armed(true);
        let mut evs = vec![press(KeyCode::Esc)];
        evs.extend(dcs_reply_events("wezterm 2.0"));
        let out = f.filter(evs);
        assert_eq!(out, vec![press(KeyCode::Esc)]);
        assert_eq!(f.take_completed().as_deref(), Some("wezterm 2.0"));
    }

    #[test]
    fn xt_filter_events_after_completion_pass_same_batch() {
        // A bare Esc (or Alt+P) right after the reply in the SAME batch
        // must come out — the disarmed filter must stop matching.
        let mut f = XtversionFilter::with_armed(true);
        let mut evs = dcs_reply_events("kitty 0.35.2");
        evs.push(press(KeyCode::Esc));
        evs.push(press_mods(
            KeyCode::Char('P'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        let out = f.filter(evs);
        assert_eq!(
            out,
            vec![
                press(KeyCode::Esc),
                press_mods(KeyCode::Char('P'), KeyModifiers::ALT | KeyModifiers::SHIFT),
            ]
        );
        assert_eq!(f.take_completed().as_deref(), Some("kitty 0.35.2"));
        assert!(!f.holding());
    }

    #[test]
    fn xt_filter_resize_mid_hold_does_not_break_reply() {
        let mut f = XtversionFilter::with_armed(true);
        let evs = dcs_reply_events("kitty 0.35.2");
        let (a, b) = evs.split_at(6);
        let resize_at = test_instant() + Duration::from_millis(3);
        let focus_at = test_instant() + Duration::from_millis(4);
        let mut first = a.to_vec();
        first.push(TimedInputEvent {
            event: Event::Resize(80, 24),
            arrived_at: resize_at,
        });
        first.push(TimedInputEvent {
            event: Event::FocusGained,
            arrived_at: focus_at,
        });
        assert!(f.filter(first).is_empty());
        assert!(f.holding());

        let out = f.filter(b.to_vec());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].event, Event::Resize(80, 24));
        assert_eq!(out[0].arrived_at, resize_at);
        assert_eq!(out[1].event, Event::FocusGained);
        assert_eq!(out[1].arrived_at, focus_at);
        assert_eq!(f.take_completed().as_deref(), Some("kitty 0.35.2"));
    }
}
