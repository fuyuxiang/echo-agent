//! CSI fragment filter for the input event channel. Sibling of [`super::xt_filter`],
//! which does the same reassembly for the XTVERSION DCS reply. See
//! [`CsiFragmentFilter`].

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use super::event_loop::{TimedInputEvent, is_bare_esc_press};

/// Persistent filter that reassembles CSI fragments leaked by crossterm when a
/// control sequence splits across `read()` boundaries — SGR mouse reports
/// `\e[<…M/m` and focus reports `\e[I`/`\e[O`. Carries state across
/// `drain_and_process` calls so a mouse report split across batches is still
/// caught (its `\x1b` in batch N, `[<…M` in batch N+1). A fragmented focus
/// report is reassembled into its `Event::FocusGained`/`Event::FocusLost` only
/// when its bare `\e` and `[I`/`[O` arrive in the same batch: a lone `\e` can't
/// be held across batches (a lone `[` must render at once), so a focus report
/// whose `\e` was isolated in a prior batch still leaks.
pub(super) struct CsiFragmentFilter {
    state: CsiFragmentState,
    tentative: Vec<TimedInputEvent>,
}

impl CsiFragmentFilter {
    pub(super) fn new() -> Self {
        Self {
            state: CsiFragmentState::Idle,
            tentative: Vec::new(),
        }
    }

    /// Process a batch of events, filtering any CSI fragments.
    /// Partial matches are held in `self.tentative` until the next call.
    /// The `esc_before_run` pop is per-call only (can't retract across batches).
    pub(super) fn filter(&mut self, events: Vec<TimedInputEvent>) -> Vec<TimedInputEvent> {
        let mut result = Vec::with_capacity(self.tentative.len() + events.len());
        let mut esc_before_run = false;
        let mut filtered_count = 0usize;

        for ev in events {
            if is_bare_esc_press(&ev.event) {
                result.append(&mut self.tentative);
                self.state = CsiFragmentState::Idle;
                result.push(ev);
                esc_before_run = true;
                continue;
            }

            match csi_filterable_char(&ev.event) {
                Some(ch) => match self.state.advance(ch) {
                    CsiAdvance::Continue(next) => {
                        self.state = next;
                        self.tentative.push(ev);
                    }
                    CsiAdvance::Complete => {
                        filtered_count += 1;
                        self.tentative.clear();
                        if esc_before_run {
                            result.pop();
                        }
                        esc_before_run = false;
                        self.state = CsiFragmentState::Idle;
                    }
                    CsiAdvance::CompleteFocus => {
                        if esc_before_run {
                            // bare \e then [I/[O in one drain batch is treated as a focus report; a typed pair rarely lands in one batch (same assumption as the mouse Complete arm)
                            filtered_count += 1;
                            self.tentative.clear();
                            result.pop(); // retract the bare Esc
                            // translate the reassembled report into its focus event so focus-driven UX (prompt refocus, recap away-timer, /gboom key-release) still fires over SSH
                            result.push(TimedInputEvent {
                                event: if ch == 'I' {
                                    Event::FocusGained
                                } else {
                                    Event::FocusLost
                                },
                                arrived_at: ev.arrived_at,
                            });
                            esc_before_run = false;
                            self.state = CsiFragmentState::Idle;
                        } else {
                            // typed `[I` / `[O` (e.g. arr[I]) — pass through
                            result.append(&mut self.tentative);
                            self.state = CsiFragmentState::Idle;
                            result.push(ev);
                        }
                    }
                    CsiAdvance::Reject => {
                        result.append(&mut self.tentative);
                        esc_before_run = false;
                        self.state = CsiFragmentState::Idle;
                        match CsiFragmentState::Idle.advance(ch) {
                            CsiAdvance::Continue(next) => {
                                self.state = next;
                                self.tentative.push(ev);
                            }
                            _ => result.push(ev),
                        }
                    }
                },
                None => {
                    result.append(&mut self.tentative);
                    self.state = CsiFragmentState::Idle;
                    esc_before_run = false;
                    result.push(ev);
                }
            }
        }

        if filtered_count > 0 {
            tracing::debug!(filtered_count, "filtered CSI fragments");
        }

        // A lone typed `[` is indistinguishable from the start of a CSI fragment —
        // an SGR mouse report `[<…M` or a focus report `[I`/`[O` — but user input
        // must render immediately. Real leaked fragments arrive with the
        // byte after `[` in the same read(); carrying only `Bracket` across batches
        // is unnecessary and holds the key until the next keystroke. Deeper partial
        // states (`[<…`) still persist for cross-batch continuation.
        if matches!(self.state, CsiFragmentState::Bracket) {
            result.append(&mut self.tentative);
            self.state = CsiFragmentState::Idle;
        }

        result
    }
}

/// States for recognizing SGR mouse `[<digits;digits;digits{M,m}` and focus `[I`/`[O`.
#[derive(Clone, Copy, Debug)]
enum CsiFragmentState {
    Idle,
    Bracket,
    LessThan,
    Digits1,
    Semi1,
    Digits2,
    Semi2,
    Digits3,
}

#[derive(Debug)]
enum CsiAdvance {
    Continue(CsiFragmentState),
    Complete,
    CompleteFocus,
    Reject,
}

impl CsiFragmentState {
    fn advance(self, ch: char) -> CsiAdvance {
        use CsiFragmentState::*;
        match (self, ch) {
            (Idle, '[') => CsiAdvance::Continue(Bracket),
            (Bracket, '<') => CsiAdvance::Continue(LessThan),
            // \e[I / \e[O focus report finals
            (Bracket, 'I') | (Bracket, 'O') => CsiAdvance::CompleteFocus,
            (LessThan | Digits1, c) if c.is_ascii_digit() => CsiAdvance::Continue(Digits1),
            (Digits1, ';') => CsiAdvance::Continue(Semi1),
            (Semi1 | Digits2, c) if c.is_ascii_digit() => CsiAdvance::Continue(Digits2),
            (Digits2, ';') => CsiAdvance::Continue(Semi2),
            (Semi2 | Digits3, c) if c.is_ascii_digit() => CsiAdvance::Continue(Digits3),
            (Digits3, 'M' | 'm') => CsiAdvance::Complete,
            _ => CsiAdvance::Reject,
        }
    }
}

fn csi_filterable_char(ev: &Event) -> Option<char> {
    match ev {
        Event::Key(ke)
            if ke.kind == KeyEventKind::Press
                && (ke.modifiers == KeyModifiers::NONE || ke.modifiers == KeyModifiers::SHIFT) =>
        {
            if let KeyCode::Char(c) = ke.code {
                Some(c)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;
    use std::time::Instant;

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

    // ── SGR mouse fragment filter tests ──────────────────────────────

    /// Build key events matching crossterm's actual output for a fragmented
    /// SGR mouse report `[<btn;col;row{M|m}]`.
    fn sgr_fragment(btn: &str, col: &str, row: &str, term: char) -> Vec<TimedInputEvent> {
        let mut events = vec![press(KeyCode::Char('[')), press(KeyCode::Char('<'))];
        for c in btn.chars() {
            events.push(press(KeyCode::Char(c)));
        }
        events.push(press(KeyCode::Char(';')));
        for c in col.chars() {
            events.push(press(KeyCode::Char(c)));
        }
        events.push(press(KeyCode::Char(';')));
        for c in row.chars() {
            events.push(press(KeyCode::Char(c)));
        }
        if term.is_uppercase() {
            events.push(press_shift(KeyCode::Char(term)));
        } else {
            events.push(press(KeyCode::Char(term)));
        }
        events
    }

    #[test]
    fn csi_filter_empty() {
        assert!(CsiFragmentFilter::new().filter(vec![]).is_empty());
    }

    #[test]
    fn csi_filter_normal_keys_unchanged() {
        let events = vec![
            press(KeyCode::Char('h')),
            press(KeyCode::Char('i')),
            press(KeyCode::Enter),
        ];
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], press(KeyCode::Char('h')));
        assert_eq!(result[1], press(KeyCode::Char('i')));
        assert_eq!(result[2], press(KeyCode::Enter));
    }

    #[test]
    fn csi_filter_single_fragment_removed() {
        let events = sgr_fragment("35", "261", "67", 'M');
        assert!(CsiFragmentFilter::new().filter(events).is_empty());
    }

    #[test]
    fn csi_filter_multiple_fragments_removed() {
        let mut events = sgr_fragment("35", "261", "67", 'M');
        events.extend(sgr_fragment("35", "263", "64", 'M'));
        assert!(CsiFragmentFilter::new().filter(events).is_empty());
    }

    #[test]
    fn csi_filter_esc_before_fragment_removed() {
        let mut events = vec![press(KeyCode::Esc)];
        events.extend(sgr_fragment("35", "261", "67", 'M'));
        assert!(CsiFragmentFilter::new().filter(events).is_empty());
    }

    #[test]
    fn csi_filter_partial_fragment_held() {
        // Partial SGR fragment (no terminating M/m) is held in the
        // persistent filter's tentative buffer, not emitted yet.
        let events = vec![
            press(KeyCode::Char('[')),
            press(KeyCode::Char('<')),
            press(KeyCode::Char('3')),
            press(KeyCode::Char('5')),
            press(KeyCode::Char(';')),
            press(KeyCode::Char('2')),
            press(KeyCode::Char('6')),
            press(KeyCode::Char('1')),
            press(KeyCode::Char(';')),
        ];
        let mut f = CsiFragmentFilter::new();
        let result = f.filter(events);
        assert!(result.is_empty(), "partial fragment should be held");
        // A follow-up non-SGR event flushes the held events.
        let result2 = f.filter(vec![press(KeyCode::Enter)]);
        assert_eq!(result2.len(), 10); // 9 held + 1 new
    }

    #[test]
    fn csi_filter_mixed_normal_and_fragment() {
        let mut events = vec![press(KeyCode::Char('h')), press(KeyCode::Char('i'))];
        events.extend(sgr_fragment("35", "261", "67", 'M'));
        events.push(press(KeyCode::Char('!')));
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], press(KeyCode::Char('h')));
        assert_eq!(result[1], press(KeyCode::Char('i')));
        assert_eq!(result[2], press(KeyCode::Char('!')));
    }

    #[test]
    fn csi_filter_lowercase_m_removed() {
        let events = sgr_fragment("35", "261", "67", 'm');
        assert!(CsiFragmentFilter::new().filter(events).is_empty());
    }

    #[test]
    fn csi_filter_non_key_events_preserved() {
        let mut events = vec![TimedInputEvent {
            event: Event::Resize(80, 24),
            arrived_at: test_instant(),
        }];
        events.extend(sgr_fragment("35", "261", "67", 'M'));
        events.push(TimedInputEvent {
            event: Event::Resize(100, 30),
            arrived_at: test_instant(),
        });
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(result.len(), 2);
        assert!(matches!(result[0].event, Event::Resize(80, 24)));
        assert!(matches!(result[1].event, Event::Resize(100, 30)));
    }

    #[test]
    fn csi_filter_esc_not_immediately_before_fragment_kept() {
        let mut events = vec![press(KeyCode::Esc), press(KeyCode::Char('x'))];
        events.extend(sgr_fragment("35", "261", "67", 'M'));
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], press(KeyCode::Esc));
        assert_eq!(result[1], press(KeyCode::Char('x')));
    }

    #[test]
    fn csi_filter_esc_and_fragment_pairs() {
        let mut events = vec![press(KeyCode::Esc)];
        events.extend(sgr_fragment("35", "261", "67", 'M'));
        events.push(press(KeyCode::Esc));
        events.extend(sgr_fragment("35", "263", "64", 'M'));
        assert!(CsiFragmentFilter::new().filter(events).is_empty());
    }

    #[test]
    fn csi_filter_reject_re_evaluates_bracket() {
        // [<35 then [ restarts; second fragment completes.
        let mut events = vec![
            press(KeyCode::Char('[')),
            press(KeyCode::Char('<')),
            press(KeyCode::Char('3')),
            press(KeyCode::Char('5')),
        ];
        events.extend(sgr_fragment("0", "0", "0", 'M'));
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(result.len(), 4); // [, <, 3, 5 preserved
    }

    /// A typed `[` must be emitted in the same batch, not held until
    /// the next keystroke (which made the cursor look stuck / "laggy").
    #[test]
    fn csi_filter_lone_bracket_emitted_same_batch() {
        let mut f = CsiFragmentFilter::new();
        let first = f.filter(vec![press(KeyCode::Char('['))]);
        assert_eq!(first, vec![press(KeyCode::Char('['))]);
        // Must not carry Bracket state into the next batch.
        let second = f.filter(vec![press(KeyCode::Char('a'))]);
        assert_eq!(second, vec![press(KeyCode::Char('a'))]);
    }

    #[test]
    fn csi_filter_min_coordinates() {
        assert!(
            CsiFragmentFilter::new()
                .filter(sgr_fragment("0", "0", "0", 'M'))
                .is_empty()
        );
    }

    #[test]
    fn csi_filter_large_coordinates() {
        assert!(
            CsiFragmentFilter::new()
                .filter(sgr_fragment("999", "9999", "9999", 'M'))
                .is_empty()
        );
    }

    #[test]
    fn csi_filter_empty_digit_field_kept() {
        // [<;1;1M — missing button digits, not a valid SGR fragment.
        let events = vec![
            press(KeyCode::Char('[')),
            press(KeyCode::Char('<')),
            press(KeyCode::Char(';')),
            press(KeyCode::Char('1')),
            press(KeyCode::Char(';')),
            press(KeyCode::Char('1')),
            press_shift(KeyCode::Char('M')),
        ];
        let mut f = CsiFragmentFilter::new();
        let result = f.filter(events);
        // The `[` starts a potential SGR match but `;` rejects at LessThan.
        // After rejection, `;` doesn't restart, so it and remaining chars
        // pass through.  The leading `[<` is flushed on reject.
        // However `[` was held in tentative while matching.  Let's just
        // verify all 7 events come out (some from this call, rest flushed
        // on the follow-up).
        let result2 = f.filter(vec![]);
        let total = result.len() + result2.len();
        assert_eq!(total, 7);
    }

    // ── Cross-batch SGR filtering tests ──────────────────────────────

    #[test]
    fn csi_filter_cross_batch_esc_then_fragment() {
        // Esc arrives in batch 1, SGR fragment chars in batch 2.
        // This is the exact scenario from the bug report.
        let mut f = CsiFragmentFilter::new();

        // Batch 1: just the Esc
        let r1 = f.filter(vec![press(KeyCode::Esc)]);
        // Esc is emitted (can't be retracted across batches)
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0], press(KeyCode::Esc));

        // Batch 2: the remaining SGR fragment chars
        let r2 = f.filter(sgr_fragment("64", "91", "51", 'M'));
        // Fragment is filtered — no garbage in the prompt
        assert!(r2.is_empty(), "SGR fragment chars should be filtered");
    }

    #[test]
    fn csi_filter_cross_batch_partial_then_rest() {
        // Fragment split mid-sequence across two batches.
        let mut f = CsiFragmentFilter::new();

        // Batch 1: partial fragment [<64;
        let r1 = f.filter(vec![
            press(KeyCode::Char('[')),
            press(KeyCode::Char('<')),
            press(KeyCode::Char('6')),
            press(KeyCode::Char('4')),
            press(KeyCode::Char(';')),
        ]);
        assert!(r1.is_empty(), "partial fragment should be held");

        // Batch 2: remaining 91;51M — uppercase M arrives with SHIFT
        // (crossterm legacy parser sets SHIFT for uppercase chars).
        let r2 = f.filter(vec![
            press(KeyCode::Char('9')),
            press(KeyCode::Char('1')),
            press(KeyCode::Char(';')),
            press(KeyCode::Char('5')),
            press(KeyCode::Char('1')),
            press_shift(KeyCode::Char('M')),
        ]);
        assert!(r2.is_empty(), "completed fragment should be filtered");
    }

    #[test]
    fn csi_filter_uppercase_m_with_shift_modifier() {
        // Regression test for the actual crossterm behavior: when a raw 'M' byte
        // (0x4D) arrives as a standalone character, crossterm's `char_code_to_event`
        // sets `KeyModifiers::SHIFT` because `'M'.is_uppercase()` is true.
        // The SGR filter must accept SHIFT-modified chars to catch these fragments.
        let events = vec![
            press(KeyCode::Esc),
            press(KeyCode::Char('[')),
            press(KeyCode::Char('<')),
            press(KeyCode::Char('6')),
            press(KeyCode::Char('4')),
            press(KeyCode::Char(';')),
            press(KeyCode::Char('1')),
            press(KeyCode::Char('1')),
            press(KeyCode::Char('2')),
            press(KeyCode::Char(';')),
            press(KeyCode::Char('6')),
            press(KeyCode::Char('3')),
            press_shift(KeyCode::Char('M')), // crossterm adds SHIFT for uppercase
        ];
        let result = CsiFragmentFilter::new().filter(events);
        assert!(
            result.is_empty(),
            "SGR fragment with SHIFT on 'M' must be filtered, got {} events",
            result.len()
        );
    }

    #[test]
    fn csi_filter_many_uppercase_m_fragments() {
        let mut events = Vec::new();
        for _ in 0..20 {
            events.push(press(KeyCode::Esc));
            events.extend(sgr_fragment("64", "112", "63", 'M'));
        }
        let result = CsiFragmentFilter::new().filter(events);
        assert!(
            result.is_empty(),
            "all SGR fragments with SHIFT-M must be filtered, got {} events",
            result.len()
        );
    }

    #[test]
    fn csi_filter_cross_batch_partial_then_reject() {
        // Partial fragment in batch 1, rejected in batch 2.
        let mut f = CsiFragmentFilter::new();

        // Batch 1: [<6
        let r1 = f.filter(vec![
            press(KeyCode::Char('[')),
            press(KeyCode::Char('<')),
            press(KeyCode::Char('6')),
        ]);
        assert!(r1.is_empty(), "partial should be held");

        // Batch 2: starts with 'a' which rejects the match
        let r2 = f.filter(vec![press(KeyCode::Char('a'))]);
        // Held events + new event are all emitted
        assert_eq!(r2.len(), 4); // [, <, 6, a
    }

    #[test]
    fn csi_filter_cross_batch_multiple_scroll_events() {
        // Multiple rapid scroll events split across batches (the exact
        // bug scenario: scrolling during worktree creation).
        let mut f = CsiFragmentFilter::new();

        // Batch 1: Esc from first scroll
        let r1 = f.filter(vec![press(KeyCode::Esc)]);
        assert_eq!(r1.len(), 1); // Esc emitted

        // Batch 2: fragment + Esc + fragment (two scroll events)
        let mut batch2 = sgr_fragment("64", "91", "51", 'M');
        batch2.push(press(KeyCode::Esc));
        batch2.extend(sgr_fragment("64", "91", "51", 'M'));
        let r2 = f.filter(batch2);
        assert!(r2.is_empty(), "all fragments and Esc should be filtered");
    }

    #[test]
    fn csi_filter_cross_batch_one_event_at_a_time() {
        // A lone typed `[` must not be held across batches, so
        // one-event-per-batch delivery of `[` alone is emitted (not filtered).
        // Real leaked fragments deliver `[<…` in the same read/batch; verify
        // that shape still filters when split only after `[<` is established.
        let mut f = CsiFragmentFilter::new();

        let r = f.filter(vec![press(KeyCode::Esc)]);
        assert_eq!(r.len(), 1);

        // Lone `[` batch — user input path, not held.
        let r = f.filter(vec![press(KeyCode::Char('['))]);
        assert_eq!(r, vec![press(KeyCode::Char('['))]);

        // Same-batch partial after `[<` is still held across batches.
        let mut f2 = CsiFragmentFilter::new();
        let partial = vec![
            press(KeyCode::Char('[')),
            press(KeyCode::Char('<')),
            press(KeyCode::Char('6')),
            press(KeyCode::Char('4')),
        ];
        assert!(f2.filter(partial).is_empty());
        let rest = vec![
            press(KeyCode::Char(';')),
            press(KeyCode::Char('9')),
            press(KeyCode::Char('1')),
            press(KeyCode::Char(';')),
            press(KeyCode::Char('5')),
            press(KeyCode::Char('1')),
            press(KeyCode::Char('M')),
        ];
        assert!(
            f2.filter(rest).is_empty(),
            "completing fragment should discard"
        );
    }

    // ── CSI focus report filtering tests ─────────────────────────────

    #[test]
    fn csi_filter_focus_timestamp_comes_from_completing_fragment() {
        let start = Instant::now();
        let complete = start + std::time::Duration::from_millis(7);
        let events = vec![
            TimedInputEvent {
                event: press(KeyCode::Esc).event,
                arrived_at: start,
            },
            TimedInputEvent {
                event: press(KeyCode::Char('[')).event,
                arrived_at: start + std::time::Duration::from_millis(3),
            },
            TimedInputEvent {
                event: press_shift(KeyCode::Char('I')).event,
                arrived_at: complete,
            },
        ];

        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].event, Event::FocusGained);
        assert_eq!(result[0].arrived_at, complete);
    }

    #[test]
    fn csi_filter_focus_in_after_esc_translated() {
        // Split \e[I focus-in (Esc, [, I — uppercase I arrives with SHIFT) is
        // reassembled into a FocusGained event, not dropped.
        let events = vec![
            press(KeyCode::Esc),
            press(KeyCode::Char('[')),
            press_shift(KeyCode::Char('I')),
        ];
        assert_eq!(
            CsiFragmentFilter::new().filter(events),
            vec![TimedInputEvent {
                event: Event::FocusGained,
                arrived_at: test_instant()
            }]
        );
    }

    #[test]
    fn csi_filter_focus_out_after_esc_translated() {
        // Split \e[O focus-out (Esc, [, O — uppercase O arrives with SHIFT) is
        // reassembled into a FocusLost event, not dropped.
        let events = vec![
            press(KeyCode::Esc),
            press(KeyCode::Char('[')),
            press_shift(KeyCode::Char('O')),
        ];
        assert_eq!(
            CsiFragmentFilter::new().filter(events),
            vec![TimedInputEvent {
                event: Event::FocusLost,
                arrived_at: test_instant()
            }]
        );
    }

    #[test]
    fn csi_filter_typed_bracket_i_kept() {
        // Typed `[I` (e.g. arr[I]) has no preceding bare Esc — pass through.
        let events = vec![press(KeyCode::Char('[')), press(KeyCode::Char('I'))];
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(
            result,
            vec![press(KeyCode::Char('[')), press(KeyCode::Char('I'))]
        );
    }

    #[test]
    fn csi_filter_typed_bracket_o_kept() {
        // Typed `[O` has no preceding bare Esc — pass through.
        let events = vec![press(KeyCode::Char('[')), press_shift(KeyCode::Char('O'))];
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(
            result,
            vec![press(KeyCode::Char('[')), press_shift(KeyCode::Char('O'))]
        );
    }

    #[test]
    fn csi_filter_ss3_not_eaten() {
        // SS3 \eOA has no `[`, so it never enters Bracket — leave it intact.
        let events = vec![
            press(KeyCode::Esc),
            press(KeyCode::Char('O')),
            press(KeyCode::Char('A')),
        ];
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(
            result,
            vec![
                press(KeyCode::Esc),
                press(KeyCode::Char('O')),
                press(KeyCode::Char('A')),
            ]
        );
    }

    #[test]
    fn csi_filter_focus_among_normal_keys() {
        // Split \e[I focus-in surrounded by typed keys — keys survive and the
        // report is translated to FocusGained in place.
        let events = vec![
            press(KeyCode::Char('a')),
            press(KeyCode::Esc),
            press(KeyCode::Char('[')),
            press_shift(KeyCode::Char('I')),
            press(KeyCode::Char('b')),
        ];
        let result = CsiFragmentFilter::new().filter(events);
        assert_eq!(
            result,
            vec![
                press(KeyCode::Char('a')),
                TimedInputEvent {
                    event: Event::FocusGained,
                    arrived_at: test_instant()
                },
                press(KeyCode::Char('b')),
            ]
        );
    }

    #[test]
    fn csi_filter_cross_batch_focus_not_retracted() {
        // known limitation: only a same-batch report is reassembled (and translated); one split across drain batches still leaks, since a lone Esc can't be held across batches
        let mut f = CsiFragmentFilter::new();
        // Batch 1: lone Esc is emitted (a lone Esc can't be held across batches).
        let r1 = f.filter(vec![press(KeyCode::Esc)]);
        assert_eq!(r1, vec![press(KeyCode::Esc)]);
        // Batch 2: `[` then SHIFT-I come through — the focus report is not retracted.
        let r2 = f.filter(vec![
            press(KeyCode::Char('[')),
            press_shift(KeyCode::Char('I')),
        ]);
        assert_eq!(
            r2,
            vec![press(KeyCode::Char('[')), press_shift(KeyCode::Char('I'))]
        );
    }
}
