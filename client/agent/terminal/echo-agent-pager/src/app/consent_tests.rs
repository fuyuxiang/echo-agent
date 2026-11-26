use super::*;
use echo_agent_runtime::util::config::{ConsentAnswer, ConsentGate};

const ACCOUNT: &str = "user@example.com";
const NOTICE_ID: &str = "tos-2026-08";
const TOS_URL: &str = "https://echo-agent.invalid/legal/terms-of-service";

fn gate() -> ConsentGate {
    ConsentGate {
        id: NOTICE_ID.to_owned(),
        version: Some(3),
        title: Some("Updated terms".to_owned()),
        body: Some(format!(
            "Review our [Terms of Service]({TOS_URL}) before continuing."
        )),
        ..Default::default()
    }
}

fn inputs<'a>(
    gate: Option<&'a ConsentGate>,
    answers: &'a BTreeMap<String, ConsentAnswer>,
) -> ConsentInputs<'a> {
    ConsentInputs {
        gate,
        answered_this_run: None,
        answers,
        account: Some(ACCOUNT),
        minimal: false,
    }
}

fn no_answers() -> BTreeMap<String, ConsentAnswer> {
    BTreeMap::new()
}

fn answered(notice_id: &str, version: i32) -> BTreeMap<String, ConsentAnswer> {
    BTreeMap::from([(
        notice_id.to_owned(),
        ConsentAnswer {
            version,
            account: Some(ACCOUNT.to_owned()),
            ..Default::default()
        },
    )])
}

/// Everything the reader sees, in order; the urls never reach the screen.
fn painted(notice: &ConsentNotice) -> String {
    notice
        .segments
        .iter()
        .map(|s| match s {
            ConsentSegment::Text(text) => text.as_str(),
            ConsentSegment::Link { label, .. } => label.as_str(),
        })
        .collect()
}

#[test]
fn valid_gate_arms_pending() {
    let gate = gate();

    let state = consent_verdict(&inputs(Some(&gate), &no_answers()));

    let ConsentState::Pending { notice, .. } = state else {
        panic!("expected pending");
    };
    assert_eq!(notice.version, 3);
    assert_eq!(notice.accept_label, "Got it");
}

#[test]
fn absent_gate_is_done() {
    assert!(matches!(
        consent_verdict(&inputs(None, &no_answers())),
        ConsentState::Done
    ));
}

/// A payload the validator refuses must leave the client usable, not block every session on it.
#[test]
fn a_refused_gate_fails_open() {
    let mut gate = gate();
    gate.body = Some(String::new());

    assert!(matches!(
        consent_verdict(&inputs(Some(&gate), &no_answers())),
        ConsentState::Done
    ));
}

/// The disk write is a spawned task, so the in-run answer is what stops a re-arm before it lands.
#[test]
fn an_answer_from_this_run_suppresses() {
    let gate = gate();
    let answers = no_answers();
    let mut i = inputs(Some(&gate), &answers);
    i.answered_this_run = Some((NOTICE_ID, 3));

    assert!(matches!(consent_verdict(&i), ConsentState::Done));
}

/// The index is what a click or a number key resolves through, so it is part of the parse.
#[test]
fn two_links_are_indexed_in_order() {
    let mut gate = gate();
    gate.body = Some(
        "Read [Terms](https://echo-agent.invalid/a) and [Policy](https://echo-agent.invalid/b)."
            .to_owned(),
    );

    let notice = ConsentNotice::try_from_remote(&gate).expect("valid");

    assert_eq!(
        notice.segments,
        vec![
            ConsentSegment::Text("Read ".to_owned()),
            ConsentSegment::Link {
                index: 0,
                label: "Terms".to_owned()
            },
            ConsentSegment::Text(" and ".to_owned()),
            ConsentSegment::Link {
                index: 1,
                label: "Policy".to_owned()
            },
            ConsentSegment::Text(".".to_owned()),
        ]
    );
    assert_eq!(
        notice.links,
        vec![
            "https://echo-agent.invalid/a",
            "https://echo-agent.invalid/b"
        ]
    );
}

/// A url we would not open costs a hyperlink, not the whole notice: the sentence still reads.
#[test]
fn a_url_we_will_not_open_degrades_to_plain_text() {
    for url in [
        "http://echo-agent.invalid/a",
        // `cmd /c start` on Windows would read the tail as a second command.
        "https://echo-agent.invalid/a&calc",
        // A percent pair is a variable to the same shell.
        "https://echo-agent.invalid/%USERNAME%",
    ] {
        let mut gate = gate();
        gate.body = Some(format!("Read [Terms]({url}) now."));

        let notice = ConsentNotice::try_from_remote(&gate).expect("valid");

        assert!(notice.links.is_empty(), "{url} must not be openable");
        assert_eq!(
            notice.segments,
            vec![
                ConsentSegment::Text("Read ".to_owned()),
                ConsentSegment::Text("Terms".to_owned()),
                ConsentSegment::Text(" now.".to_owned()),
            ],
            "{url} must leave the sentence intact",
        );
    }
}

/// A label of spaces paints nothing, so its number key would open a url out of nowhere.
#[test]
fn a_link_with_nothing_to_paint_is_dropped() {
    let mut gate = gate();
    gate.body = Some(
        "Read [ ](https://echo-agent.invalid/a) and [Terms](https://echo-agent.invalid/b)."
            .to_owned(),
    );

    let notice = ConsentNotice::try_from_remote(&gate).expect("valid");

    assert_eq!(notice.links, vec!["https://echo-agent.invalid/b"]);
    // The surviving link takes index 0, so the dropped one leaves no gap for a key to fall into.
    assert_eq!(
        notice.segments,
        vec![
            ConsentSegment::Text("Read ".to_owned()),
            ConsentSegment::Text(" and ".to_owned()),
            ConsentSegment::Link {
                index: 0,
                label: "Terms".to_owned()
            },
            ConsentSegment::Text(".".to_owned()),
        ]
    );
}

#[test]
fn empty_body_refuses() {
    let mut gate = gate();
    gate.body = Some("   ".to_owned());

    assert_eq!(
        ConsentNotice::try_from_remote(&gate),
        Err(ConsentArmRefusal::EmptyBody)
    );
}

#[test]
fn missing_version_refuses() {
    let mut gate = gate();
    gate.version = None;

    assert_eq!(
        ConsentNotice::try_from_remote(&gate),
        Err(ConsentArmRefusal::MissingVersion)
    );
}

#[test]
fn implausible_version_refuses() {
    let mut gate = gate();
    gate.version = Some(999_999);

    assert_eq!(
        ConsentNotice::try_from_remote(&gate),
        Err(ConsentArmRefusal::ImplausibleVersion(999_999))
    );
}

#[test]
fn escapes_and_reordering_characters_are_stripped_from_the_body() {
    let mut gate = gate();
    gate.body = Some("before\u{1b}[31m\u{202e}\u{200b}after".to_owned());

    let notice = ConsentNotice::try_from_remote(&gate).expect("valid");

    let painted: String = notice
        .segments
        .iter()
        .map(|s| match s {
            ConsentSegment::Text(t) => t.as_str(),
            ConsentSegment::Link { label, .. } => label.as_str(),
        })
        .collect();
    assert_eq!(painted, "before[31mafter");
}

/// Not conditional on the server ack, or a missing backend would re-ask someone who answered.
#[test]
fn an_unacked_answer_at_or_above_the_version_suppresses() {
    let gate = gate();
    let answers = answered(NOTICE_ID, 3);
    assert!(!answers[NOTICE_ID].acked);

    let verdict = consent_verdict(&inputs(Some(&gate), &answers));

    assert!(matches!(verdict, ConsentState::Done));
}

#[test]
fn older_answer_does_not_suppress_newer_notice() {
    let gate = gate();

    let verdict = consent_verdict(&inputs(Some(&gate), &answered(NOTICE_ID, 2)));

    assert!(matches!(verdict, ConsentState::Pending { .. }));
}

/// Version counters run per notice id, so a high answer to one must not cover another.
#[test]
fn answer_to_a_different_notice_does_not_suppress() {
    let gate = gate();

    let verdict = consent_verdict(&inputs(Some(&gate), &answered("consumer-tos-2026-08", 9)));

    assert!(matches!(verdict, ConsentState::Pending { .. }));
}

#[test]
fn answer_from_another_user_does_not_suppress() {
    let gate = gate();
    let mut answers = answered(NOTICE_ID, 3);
    answers.get_mut(NOTICE_ID).unwrap().account = Some("someone-else@example.com".to_owned());

    let verdict = consent_verdict(&inputs(Some(&gate), &answers));

    assert!(matches!(verdict, ConsentState::Pending { .. }));
}

/// An api key carries no email. An answer filed under no account belongs to nobody in particular,
/// so it must not answer for the next key-authenticated user on the machine.
#[test]
fn an_accountless_answer_does_not_suppress() {
    let gate = gate();
    let mut answers = answered(NOTICE_ID, 3);
    answers.get_mut(NOTICE_ID).unwrap().account = None;
    let mut i = inputs(Some(&gate), &answers);
    i.account = None;

    assert!(matches!(consent_verdict(&i), ConsentState::Pending { .. }));
}

#[test]
fn minimal_mode_fails_open() {
    let gate = gate();
    let answers = no_answers();
    let mut i = inputs(Some(&gate), &answers);
    i.minimal = true;

    assert!(matches!(consent_verdict(&i), ConsentState::Done));
}

#[test]
fn a_body_taller_than_a_standard_terminal_refuses() {
    let mut gate = gate();
    gate.body = Some("line\n".repeat(40));

    assert!(matches!(
        ConsentNotice::try_from_remote(&gate),
        Err(ConsentArmRefusal::BodyTooTall(_))
    ));
}

#[test]
fn an_unusable_id_refuses() {
    for id in ["", "has spaces", &"x".repeat(100), "quote\"inject"] {
        let mut gate = gate();
        gate.id = id.to_owned();

        assert_eq!(
            ConsentNotice::try_from_remote(&gate),
            Err(ConsentArmRefusal::UnusableId),
            "{id:?} must not arm the gate",
        );
    }
}

/// Markup the parser cannot pair off would have to be painted or dropped. Painting shows a url,
/// dropping serves a legal notice with a sentence missing, so neither is on offer.
#[test]
fn unpairable_markup_refuses() {
    for body in [
        // The byte cap can cut a link mid-url.
        "Read our [Terms](https://echo-agent.invalid/legal/te",
        // A url holds no space, so this `)` closes the second link, not the first.
        "Read [Terms](https://echo-agent.invalid/a [Policy](https://echo-agent.invalid/b) now.",
        // The same, with the two links flush against each other and no space to give it away.
        "Read [Terms](https://echo-agent.invalid/a[Policy](https://echo-agent.invalid/b) now.",
        // A `]` too many desyncs the pairing and leaves the url with no `[` in front of it.
        "[a] b](https://echo-agent.invalid/legal/tos) applies.",
    ] {
        let mut gate = gate();
        gate.body = Some(body.to_owned());

        assert_eq!(
            ConsentNotice::try_from_remote(&gate),
            Err(ConsentArmRefusal::UnpairedMarkup),
            "{body:?} must not arm the gate",
        );
    }
}

/// Terminals linkify a bare url on sight, so one outside a link would open without ever passing
/// the checks a link's url goes through.
#[test]
fn a_url_outside_a_link_refuses() {
    let mut gate = gate();
    gate.body = Some(
        "Review the terms at https://echo-agent.invalid/legal/tos before continuing.".to_owned(),
    );

    assert_eq!(
        ConsentNotice::try_from_remote(&gate),
        Err(ConsentArmRefusal::UrlInPlainText)
    );
}

/// A body that reduces to nothing readable must refuse, not arm a notice with no text: the screen
/// withholds accept for a body it did not paint, so the user could only ever quit.
#[test]
fn a_body_that_paints_nothing_refuses() {
    for body in [
        "[](https://echo-agent.invalid/legal/tos)",
        // Combining marks survive sanitizing and occupy no columns.
        "\u{0301}\u{0301}\u{0301}",
        // A space between them makes the row non-empty without making it readable.
        "\u{0301} \u{0301}",
    ] {
        let mut gate = gate();
        gate.body = Some(body.to_owned());

        assert_eq!(
            ConsentNotice::try_from_remote(&gate),
            Err(ConsentArmRefusal::EmptyBody),
            "{body:?} must not arm the gate",
        );
    }
}

/// Whatever the body does, no url and no raw markup may reach the screen, and no link may point
/// somewhere the label does not say.
#[test]
fn no_body_shape_leaks_a_url_or_raw_markup() {
    let bodies = [
        "[](https://echo-agent.invalid/legal/tos) applies.",
        "[note] see [AUP](https://echo-agent.invalid/legal/aup).",
        "Read [Terms](javascript:alert(1)) now.",
        "Read [Terms](https://echo-agent.invalid/a&calc) now.",
        // A `)` inside the url ends it early and leaves the rest to be painted as prose.
        "Read [Terms](https://echo-agent.invalid/a(b)https://evil.example) now.",
    ];

    for body in bodies {
        let mut gate = gate();
        gate.body = Some(body.to_owned());

        let Ok(notice) = ConsentNotice::try_from_remote(&gate) else {
            // Refusing is the other safe answer: nothing painted leaks nothing.
            continue;
        };

        let painted = painted(&notice);
        assert!(!painted.contains("http"), "{body:?} painted {painted:?}");
        assert!(!painted.contains("]("), "{body:?} painted {painted:?}");
        assert!(
            notice.links.iter().all(|url| is_paintable_url(url)),
            "{body:?} kept {:?}",
            notice.links,
        );
    }
}
