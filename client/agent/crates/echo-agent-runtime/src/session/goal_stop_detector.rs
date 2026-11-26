//! Heuristic stop-detector for premature "give up" turn endings.
//!
//! The model is judged to be bailing out when the LAST non-empty
//! paragraph of its turn-final text starts with one of the patterns
//! commonly used as a bail / hand-off / verdict signal.
//! On a hit, `maybe_queue_goal_continuation` (reached on the success /
//! continuation path) renders the bail-specific continuation nudge
//! instead of the generic one and the harness emits
//! `Event::GoalPrematureStopDetected { pattern }` tagged with the
//! matched pattern label so dashboards can audit precision / recall of
//! the regex panel.
//!
//! Each regex is locked to a source-string constant in
//! [`STOP_REGEX_SOURCES`] by a regression test (asserting each
//! `Regex::as_str()` matches) so a later refactor cannot silently
//! swap a pattern out. Two patterns are expressed in two stages
//! rather than as a single regex:
//!
//! 1. [`CHECK_BACK_LATER`] — a single-regex form would need a negative
//!    lookahead `(?!your?\b)` that the `regex` crate does not
//!    support; we implement the same semantics in two stages
//!    (broad first-stage regex + `your`/`you` post-filter in
//!    `check_back_later_matches`).
//! 2. `STOPPING_HERE` — the base trailer set
//!    `(?:\.|$| \u2014| -| until| pending| since| because)` is
//!    widened to include `,`, `;`, and ` for ` so naturally-occurring
//!    sign-offs like "Stopping here for now.", "Stopping here, will
//!    come back later.", and "Paused here; review needed." still
//!    fire. The widening is bounded by a non-letter boundary in the
//!    tests so in-word matches like "Stopping hereafter" stay
//!    rejected.
//!
//! Intentionally omitted from this panel: a broad catch-all
//! "continuation deferral" pattern (`\b(?:once|when|after|until|
//! as soon as)\b…`). Stand-alone it fires on routine work narration
//! ("Once the test settles I'll iterate") and the resulting false-
//! positive rate dwarfs the bail signal we care about; the more
//! specific patterns below already cover the bail surface.
//!
//! Regexes are anchored with `^` so the marker must start a line; in-
//! prose mentions like "I can't continue without your input" mid-
//! sentence are intentionally ignored.

use regex::Regex;
use std::sync::LazyLock;

/// Stable label for `Event::GoalPrematureStopDetected.pattern` when
/// the surrender-phrasing regex fires.
pub(crate) const PATTERN_UNABLE_TO_PROCEED: &str = "unable_to_proceed";
/// Stable label for "giving up" / "task not actionable".
pub(crate) const PATTERN_GIVING_UP: &str = "giving_up";
/// Stable label for "Stopping here" / "Parked the branch" /
/// "Paused here" family.
pub(crate) const PATTERN_STOPPING_HERE: &str = "stopping_here";
/// Stable label for the "N agents in flight" / loop-active /
/// "Waiting for the cron" hand-off family.
pub(crate) const PATTERN_AGENTS_IN_FLIGHT: &str = "agents_in_flight";
/// Stable label for the "I'll check back / retry later" deferral
/// pattern (`CHECK_BACK_LATER`).
pub(crate) const PATTERN_CHECK_BACK_LATER: &str = "check_back_later";
/// Stable label for the `VERDICT: PASS|FAIL` self-sign-off.
pub(crate) const PATTERN_VERDICT_LINE: &str = "verdict_line";
/// Stable label for the commit / push / PR-opened hand-off family.
pub(crate) const PATTERN_COMMIT_PUSH_PR: &str = "commit_push_pr";
/// Stable label for the "Ready for review / to merge / to ship"
/// hand-off.
pub(crate) const PATTERN_READY_FOR_REVIEW: &str = "ready_for_review";
/// Stable label for the "Please <verb> X for me" user-deflection
/// pattern.
pub(crate) const PATTERN_PLEASE_DEFLECTION: &str = "please_deflection";

const UNABLE_TO_PROCEED_SRC: &str = r"^I (?:can(?:'?t|not)|am unable to) (?:proceed|continue|make (?:any )?progress|complete|fix this)\b";
const GIVING_UP_SRC: &str = r"^(?:Giving up|I(?:'m| am) giving up|The task is not actionable)\b";
const STOPPING_HERE_SRC: &str = r"^(?:Stopping here|I've stopped here|Parked (?:the|this) branch|Paused here)(?:\.|,|;|$| for | \u{2014}| -| until| pending| since| because)";
const AGENTS_IN_FLIGHT_SRC: &str = r"^(?:(?:\*\*)?[1-9]\d* (?:agent|cron|task|fork|job|worker|PR|check)s? (?:in flight|remaining|active|still (?:running|working)|pending|running|launched)\b|(?:Continuous )?(?:[Ll]oop|[Cc]rons?|[Bb]abysit) (?:active|healthy|continuing|running|will keep|continues)\b|Waiting for (?:the )?(?:agent|cron|task|fork|worker|job|remaining|them)s?\b|Agents? will report back\b|Waiting\.?$)";
/// First-stage regex for `CHECK_BACK_LATER`. Captures the trailing
/// token after `when|once|after|until` so the post-filter can decide
/// whether the deferral target is the user (`you`/`your`) or the
/// system (anything else). `in`/`again` branches are unconditional —
/// they are never a deferral back to the user.
const CHECK_BACK_LATER_BROAD_SRC: &str = r"^(?:I will|I'll|Will) (?:check back|re-?check|poll|look again|retry|re-?run|try again) (?:in\b|again\b|(?:when|once|after|until)\s+(\S+))";
const VERDICT_LINE_SRC: &str = r"^VERDICT: (?:PASS|FAIL)\b";
const COMMIT_PUSH_PR_SRC: &str = r"^(?:Pushed (?:to `|`[0-9a-f]{7,})|Committed as `?[0-9a-f]{7,}\b|Commit: `?[0-9a-f]{7,}\b|(?:Opened|Created) PR #?\d)";
const READY_FOR_REVIEW_SRC: &str = r"^Ready (?:for review|to (?:upload|merge|ship|land))\b";
const PLEASE_DEFLECTION_SRC: &str = r"^Please (?:start|run|provide|grant|export|add|install|configure|give me|paste|point me|set (?:the |up |`?[A-Z][A-Z0-9_]+\b))";

static UNABLE_TO_PROCEED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(UNABLE_TO_PROCEED_SRC).expect("UNABLE_TO_PROCEED must compile"));
static GIVING_UP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(GIVING_UP_SRC).expect("GIVING_UP must compile"));
static STOPPING_HERE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(STOPPING_HERE_SRC).expect("STOPPING_HERE must compile"));
static AGENTS_IN_FLIGHT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(AGENTS_IN_FLIGHT_SRC).expect("AGENTS_IN_FLIGHT must compile"));
static CHECK_BACK_LATER_BROAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(CHECK_BACK_LATER_BROAD_SRC).expect("CHECK_BACK_LATER_BROAD must compile")
});
static VERDICT_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(VERDICT_LINE_SRC).expect("VERDICT_LINE must compile"));
static COMMIT_PUSH_PR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(COMMIT_PUSH_PR_SRC).expect("COMMIT_PUSH_PR must compile"));
static READY_FOR_REVIEW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(READY_FOR_REVIEW_SRC).expect("READY_FOR_REVIEW must compile"));
static PLEASE_DEFLECTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(PLEASE_DEFLECTION_SRC).expect("PLEASE_DEFLECTION must compile"));

/// `CHECK_BACK_LATER` post-filter: the broad regex captures the
/// trailing token after `when|once|after|until`; the equivalent
/// negative-lookahead `(?!your?\b)` means "the trailing
/// token does not start a `your?` word". `you`/`your` followed by a
/// word-boundary character (anything outside `[A-Za-z0-9_]`) is a
/// deferral to the user, which should NOT be flagged as a self-bail.
fn check_back_later_matches(line: &str) -> bool {
    let Some(caps) = CHECK_BACK_LATER_BROAD.captures(line) else {
        return false;
    };
    let Some(target) = caps.get(1) else {
        // `in` / `again` branches have no capture — always a bail.
        return true;
    };
    let token = target.as_str();
    !is_user_pronoun(token)
}

/// True iff `token` is a `\byour?\b`-matching word — i.e. `you` or
/// `your` immediately followed by a non-word character (or
/// end-of-string). Anything longer (`yours`, `youthful`, …) is not
/// the negative branch.
fn is_user_pronoun(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    for stem in ["your", "you"] {
        if let Some(rest) = lower.strip_prefix(stem)
            && rest
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
        {
            return true;
        }
    }
    false
}

/// Source string for each regex. Locked against the live
/// `Regex::as_str()` value in `regex_sources_match_compiled_regex`.
/// Each tuple is `(label, source)`.
#[cfg(test)]
const STOP_REGEX_SOURCES: &[(&str, &str)] = &[
    (PATTERN_UNABLE_TO_PROCEED, UNABLE_TO_PROCEED_SRC),
    (PATTERN_GIVING_UP, GIVING_UP_SRC),
    (PATTERN_STOPPING_HERE, STOPPING_HERE_SRC),
    (PATTERN_AGENTS_IN_FLIGHT, AGENTS_IN_FLIGHT_SRC),
    (PATTERN_CHECK_BACK_LATER, CHECK_BACK_LATER_BROAD_SRC),
    (PATTERN_VERDICT_LINE, VERDICT_LINE_SRC),
    (PATTERN_COMMIT_PUSH_PR, COMMIT_PUSH_PR_SRC),
    (PATTERN_READY_FOR_REVIEW, READY_FOR_REVIEW_SRC),
    (PATTERN_PLEASE_DEFLECTION, PLEASE_DEFLECTION_SRC),
];

/// Per-line matcher: returns `true` iff `line` matches a given
/// labelled pattern. Lets us share the same dispatch table between
/// `matched_stop_pattern` and `looks_like_premature_stop` without
/// re-implementing the `CHECK_BACK_LATER` post-filter.
fn line_matches(label: &'static str, line: &str) -> bool {
    match label {
        PATTERN_UNABLE_TO_PROCEED => UNABLE_TO_PROCEED.is_match(line),
        PATTERN_GIVING_UP => GIVING_UP.is_match(line),
        PATTERN_STOPPING_HERE => STOPPING_HERE.is_match(line),
        PATTERN_AGENTS_IN_FLIGHT => AGENTS_IN_FLIGHT.is_match(line),
        PATTERN_CHECK_BACK_LATER => check_back_later_matches(line),
        PATTERN_VERDICT_LINE => VERDICT_LINE.is_match(line),
        PATTERN_COMMIT_PUSH_PR => COMMIT_PUSH_PR.is_match(line),
        PATTERN_READY_FOR_REVIEW => READY_FOR_REVIEW.is_match(line),
        PATTERN_PLEASE_DEFLECTION => PLEASE_DEFLECTION.is_match(line),
        _ => false,
    }
}

/// Stable list of pattern labels in declaration order. Iterated by
/// the dispatch in [`matched_stop_pattern`].
const PATTERN_LABELS: &[&str] = &[
    PATTERN_UNABLE_TO_PROCEED,
    PATTERN_GIVING_UP,
    PATTERN_STOPPING_HERE,
    PATTERN_AGENTS_IN_FLIGHT,
    PATTERN_CHECK_BACK_LATER,
    PATTERN_VERDICT_LINE,
    PATTERN_COMMIT_PUSH_PR,
    PATTERN_READY_FOR_REVIEW,
    PATTERN_PLEASE_DEFLECTION,
];

/// Returns the first matched pattern label when the LAST non-empty
/// paragraph of `text` contains a line that triggers any of the
/// reference stop patterns. Order is declaration order:
/// `unable_to_proceed → giving_up → stopping_here → agents_in_flight
/// → check_back_later → verdict_line → commit_push_pr →
/// ready_for_review → please_deflection`.
///
/// Matching contract:
/// * `\r\n` line endings are normalised to `\n` on entry so CRLF
///   paragraphs split the same way LF paragraphs do.
/// * Whitespace at the start/end of `text` is ignored.
/// * "Paragraph" = consecutive non-blank lines; the *last* such block
///   is the only one considered. Earlier-paragraph hits do not fire.
/// * Inside the last paragraph, every line is trimmed and matched
///   against each pattern individually. The patterns are `^`-anchored,
///   so the marker must start a line: "I can't continue without your
///   input" inside a sentence does NOT match.
pub(crate) fn matched_stop_pattern(text: &str) -> Option<&'static str> {
    let normalised = normalise_line_endings(text);
    let last_paragraph = last_non_empty_paragraph(&normalised)?;
    let label = last_paragraph
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find_map(|line| {
            PATTERN_LABELS
                .iter()
                .copied()
                .find(|label| line_matches(label, line))
        })?;
    Some(label)
}

/// Convenience wrapper around [`matched_stop_pattern`]; returns
/// `true` iff a pattern matched. Used by the boolean-only test
/// assertions; production callers use `matched_stop_pattern` so the
/// pattern label can be threaded into `Event::GoalPrematureStopDetected`.
#[cfg(test)]
fn looks_like_premature_stop(text: &str) -> bool {
    matched_stop_pattern(text).is_some()
}

/// Normalise `\r\n` (and bare `\r`) to `\n`. Allocates only when the
/// input contains a `\r`; LF-only text is returned borrowed.
fn normalise_line_endings(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\r') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Return the last non-empty paragraph of `text`, where paragraphs are
/// separated by one or more blank lines. `None` when `text` has no
/// non-whitespace content.
fn last_non_empty_paragraph(text: &str) -> Option<&str> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unable_to_proceed_canonical_phrases_trigger() {
        for phrase in [
            "I can't proceed.",
            "I cannot continue.",
            "I can't make any progress here.",
            "I am unable to complete this task.",
            "I cant fix this without help.",
        ] {
            assert_eq!(
                matched_stop_pattern(phrase),
                Some(PATTERN_UNABLE_TO_PROCEED),
                "should flag bail phrase: {phrase}",
            );
        }
    }

    #[test]
    fn giving_up_phrases_trigger() {
        for phrase in [
            "Giving up.",
            "I'm giving up on this branch.",
            "I am giving up.",
            "The task is not actionable as stated.",
        ] {
            assert_eq!(
                matched_stop_pattern(phrase),
                Some(PATTERN_GIVING_UP),
                "should flag giving-up phrase: {phrase}",
            );
        }
    }

    #[test]
    fn stopping_here_phrases_trigger() {
        for phrase in [
            "Stopping here.",
            "I've stopped here pending review.",
            "Parked the branch until you confirm.",
            "Paused here because the gate failed.",
            "Stopping here for now.",
            "Stopping here, will come back later.",
            "Paused here; review needed.",
        ] {
            assert_eq!(
                matched_stop_pattern(phrase),
                Some(PATTERN_STOPPING_HERE),
                "should flag stopping-here phrase: {phrase}",
            );
        }
        // In-word boundaries must still reject: `hereafter`, `forever`
        // (no space after `for`) are not the widened trailer set.
        assert!(matched_stop_pattern("Stopping hereafter we ship").is_none());
        assert!(
            matched_stop_pattern("Stopping here forever, I quit.").is_none(),
            "`for` without trailing space must NOT match the ` for ` trailer",
        );
    }

    #[test]
    fn agents_in_flight_phrases_trigger() {
        for phrase in [
            "3 agents in flight.",
            "2 PRs remaining.",
            "Loop active.",
            "Continuous loop continuing.",
            "Waiting for the cron.",
            "Agents will report back.",
            "Waiting.",
        ] {
            assert_eq!(
                matched_stop_pattern(phrase),
                Some(PATTERN_AGENTS_IN_FLIGHT),
                "should flag hand-off phrase: {phrase}",
            );
        }
    }

    #[test]
    fn verdict_line_triggers() {
        assert_eq!(
            matched_stop_pattern("VERDICT: PASS"),
            Some(PATTERN_VERDICT_LINE),
        );
        assert_eq!(
            matched_stop_pattern("VERDICT: FAIL"),
            Some(PATTERN_VERDICT_LINE),
        );
        assert!(matched_stop_pattern("VERDICT: maybe").is_none());
    }

    #[test]
    fn check_back_later_basic_phrases_trigger() {
        for phrase in [
            "I'll check back in 5 minutes.",
            "I will retry once the build is green.",
            "I'll re-run when the queue clears.",
            "Will poll again in a bit.",
        ] {
            assert_eq!(
                matched_stop_pattern(phrase),
                Some(PATTERN_CHECK_BACK_LATER),
                "should flag check-back phrase: {phrase}",
            );
        }
    }

    /// Every non-user trailing token crossed with every deferral
    /// conjunction (`when|once|after|until`) must trigger — guards
    /// against silently re-narrowing the post-filter to a small
    /// allow-list and dropping legitimate bail subjects.
    #[test]
    fn check_back_later_walks_all_non_user_targets() {
        for conjunction in ["when", "once", "after", "until"] {
            for target in [
                "the build",
                "it settles",
                "this lands",
                "that passes",
                "they merge",
                "I retry",
                "tests pass",
                "CI is green",
                "we deploy",
                "stuff lands",
                "Susan signs off",
            ] {
                let line = format!("I'll retry {conjunction} {target}.");
                assert_eq!(
                    matched_stop_pattern(&line),
                    Some(PATTERN_CHECK_BACK_LATER),
                    "should flag {line}",
                );
            }
        }
    }

    /// Deferrals back to the user (`you` / `your` with either case)
    /// must NOT trigger. Trailing word characters (`yours`,
    /// `your_team`) keep the `\byour?\b` boundary unmet and are
    /// covered by `check_back_later_user_pronoun_requires_word_boundary`.
    #[test]
    fn check_back_later_does_not_flag_user_deferrals() {
        for line in [
            "I'll check back when your patch lands.",
            "I'll check back when you confirm.",
            "I'll retry once Your team reviews.",
            "I will re-run after YOU sign off.",
        ] {
            assert!(
                matched_stop_pattern(line).is_none(),
                "user deferrals must not fire: {line}",
            );
        }
    }

    /// `yours` / `your_team` / `youthful` are NOT the negative branch:
    /// `your?` requires a `\b` boundary, and word characters (`s`,
    /// `_`, `t`) keep the regex from matching. The post-filter
    /// treats them like any other non-pronoun target.
    #[test]
    fn check_back_later_user_pronoun_requires_word_boundary() {
        for line in [
            "I'll check back when yours arrives.",
            "I'll check back when your_team approves.",
            "I'll check back when youthful errors return.",
        ] {
            assert_eq!(
                matched_stop_pattern(line),
                Some(PATTERN_CHECK_BACK_LATER),
                "non-pronoun trailer must fire as a bail: {line}",
            );
        }
    }

    #[test]
    fn commit_push_pr_phrases_trigger() {
        for phrase in [
            "Pushed to `feature/branch`",
            "Pushed to `abcdef1234567`",
            "Committed as `abcdef1234`",
            "Commit: abcdef1234",
            "Opened PR #123",
            "Created PR #4567",
        ] {
            assert_eq!(
                matched_stop_pattern(phrase),
                Some(PATTERN_COMMIT_PUSH_PR),
                "should flag commit/push/PR hand-off: {phrase}",
            );
        }
        // Short hex (<7) must not fire on the hex-bearing branches —
        // that's the `[0-9a-f]{7,}` length guard. The `Pushed`
        // alternation has two arms: `Pushed to \`` (literal — any
        // branch name) and `Pushed \`[0-9a-f]{7,}` (backtick + hex).
        // The negatives below target only the hex-bearing arms.
        assert!(matched_stop_pattern("Commit: abc").is_none());
        assert!(
            matched_stop_pattern("Pushed `abc`").is_none(),
            "<7 hex must not fire on the Pushed-without-`to` backtick branch",
        );
        assert!(
            matched_stop_pattern("Committed as abcde").is_none(),
            "<7 hex must not fire on the Committed branch",
        );
        assert!(
            matched_stop_pattern("Opened PR").is_none(),
            "no number after PR must not fire",
        );
    }

    #[test]
    fn ready_for_review_phrases_trigger() {
        for phrase in [
            "Ready for review.",
            "Ready to merge.",
            "Ready to ship soon.",
            "Ready to land tomorrow.",
            "Ready to upload now.",
        ] {
            assert_eq!(
                matched_stop_pattern(phrase),
                Some(PATTERN_READY_FOR_REVIEW),
                "should flag ready-for-X hand-off: {phrase}",
            );
        }
        assert!(matched_stop_pattern("Ready to work on the next item").is_none());
    }

    #[test]
    fn please_deflection_phrases_trigger() {
        for phrase in [
            "Please start the deploy.",
            "Please run the migrations.",
            "Please provide the credentials.",
            "Please grant access.",
            "Please export the dump.",
            "Please add the env var.",
            "Please install Docker.",
            "Please configure SSO.",
            "Please give me the token.",
            "Please paste the log.",
            "Please point me at the source.",
            "Please set the ECHO_AGENT_API_KEY.",
            "Please set up the cluster.",
            "Please set the auth header.",
        ] {
            assert_eq!(
                matched_stop_pattern(phrase),
                Some(PATTERN_PLEASE_DEFLECTION),
                "should flag please-deflection: {phrase}",
            );
        }
        assert!(matched_stop_pattern("Please review the PR when you have time").is_none());
    }

    #[test]
    fn mid_sentence_phrasing_does_not_trigger() {
        let text = "Although I can't continue here without confirmation, \
                    I will keep iterating in the next turn.";
        assert!(
            !looks_like_premature_stop(text),
            "patterns are ^-anchored; mid-sentence mention must not fire",
        );
    }

    #[test]
    fn trailing_whitespace_is_tolerated() {
        assert!(looks_like_premature_stop("Giving up.   \n   \n"));
        assert!(looks_like_premature_stop("   Giving up.\n"));
    }

    #[test]
    fn last_paragraph_match_triggers_when_earlier_paragraphs_innocuous() {
        let text = "Wrote the fix and added a regression test.\n\
                    Running the suite now.\n\n\
                    \n\n\
                    Giving up.";
        assert!(looks_like_premature_stop(text));
    }

    #[test]
    fn earlier_paragraph_match_does_not_trigger() {
        let text = "Giving up on the old plan.\n\n\
                    Switched to the new one and finished the integration test. \
                    Re-running the suite to confirm.";
        assert!(!looks_like_premature_stop(text));
    }

    /// CRLF (`\r\n\r\n`) line endings must split paragraphs
    /// identically to LF; mixed and bare-CR inputs are also tolerated.
    #[test]
    fn crlf_line_endings_do_not_break_paragraph_split() {
        let crlf = "Wrote the fix.\r\n\r\nGiving up.\r\n";
        assert_eq!(
            matched_stop_pattern(crlf),
            Some(PATTERN_GIVING_UP),
            "CRLF last paragraph must still fire",
        );
        let mixed = "Giving up on the old plan.\r\n\r\n\
                     Switched to the new one and finished the integration test.\n";
        assert!(
            !looks_like_premature_stop(mixed),
            "CRLF earlier-paragraph bail must NOT fire: {mixed}",
        );
        let bare_cr = "work\rGiving up.\r";
        assert_eq!(
            matched_stop_pattern(bare_cr),
            Some(PATTERN_GIVING_UP),
            "bare CR must normalise into a line break",
        );
    }

    #[test]
    fn empty_input_does_not_trigger() {
        assert!(!looks_like_premature_stop(""));
        assert!(!looks_like_premature_stop("   \n\n  \n"));
    }

    #[test]
    fn ordinary_progress_narration_does_not_trigger() {
        for phrase in [
            "Implemented the helper and wired it through the planner.",
            "Tests pass on the fast path; one flake remains in the slow path.",
            "Ran cargo fmt and clippy; both are clean.",
            "Next: extend the gate to cover the resume site.",
        ] {
            assert!(
                !looks_like_premature_stop(phrase),
                "progress narration must not fire: {phrase}",
            );
        }
    }

    #[test]
    fn multi_line_paragraph_any_line_can_match() {
        let text = "Update on the run:\n\
                    Tests are green.\n\
                    Giving up on the doc rewrite.";
        assert_eq!(matched_stop_pattern(text), Some(PATTERN_GIVING_UP));
    }

    /// Each compiled regex's `as_str()` must equal its source
    /// string in `STOP_REGEX_SOURCES` — locks the pattern set so a
    /// future refactor that swaps a regex cannot silently drift the
    /// panel.
    #[test]
    fn regex_sources_match_compiled_regex() {
        for (label, src) in STOP_REGEX_SOURCES {
            let live = match *label {
                PATTERN_UNABLE_TO_PROCEED => UNABLE_TO_PROCEED.as_str(),
                PATTERN_GIVING_UP => GIVING_UP.as_str(),
                PATTERN_STOPPING_HERE => STOPPING_HERE.as_str(),
                PATTERN_AGENTS_IN_FLIGHT => AGENTS_IN_FLIGHT.as_str(),
                PATTERN_CHECK_BACK_LATER => CHECK_BACK_LATER_BROAD.as_str(),
                PATTERN_VERDICT_LINE => VERDICT_LINE.as_str(),
                PATTERN_COMMIT_PUSH_PR => COMMIT_PUSH_PR.as_str(),
                PATTERN_READY_FOR_REVIEW => READY_FOR_REVIEW.as_str(),
                PATTERN_PLEASE_DEFLECTION => PLEASE_DEFLECTION.as_str(),
                other => panic!("unexpected label {other}"),
            };
            assert_eq!(
                live, *src,
                "{label}: compiled regex must equal its source string",
            );
        }
    }

    /// The `PATTERN_LABELS` dispatch table must enumerate every
    /// entry in `STOP_REGEX_SOURCES` in declaration order — guards
    /// against forgetting to add a new pattern to one of the two
    /// lists.
    #[test]
    fn pattern_labels_match_source_table_order() {
        let from_sources: Vec<&'static str> =
            STOP_REGEX_SOURCES.iter().map(|(label, _)| *label).collect();
        assert_eq!(PATTERN_LABELS, &from_sources[..]);
    }
}
