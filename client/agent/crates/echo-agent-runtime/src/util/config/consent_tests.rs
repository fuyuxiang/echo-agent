use super::*;
use toml::Value as TomlValue;

/// A single slot would drop one of two live notices and re-prompt forever.
#[test]
fn consent_answers_are_kept_per_notice() {
    let root: TomlValue = toml::from_str(
        r#"
[consent.answers."enterprise-tos-2026-08"]
version = 2
account = "user@example.com"

[consent.answers."consumer-tos-2026-08"]
version = 1
account = "other@example.com"
"#,
    )
    .unwrap();

    let consent = super::super::load_config_from_toml(&root).consent;

    assert_eq!(consent.answers["enterprise-tos-2026-08"].version, 2);
    assert_eq!(
        consent.answers["consumer-tos-2026-08"].account.as_deref(),
        Some("other@example.com")
    );

    let emitted = toml::to_string(&consent).unwrap();
    let reparsed: ConsentConfig = toml::from_str(&emitted).unwrap();
    assert_eq!(reparsed, consent);
}

#[tokio::test]
#[serial_test::serial(ECHO_AGENT_HOME)]
async fn set_consent_answer_is_monotonic_per_account() {
    let home = tempfile::tempdir().expect("home");
    let _guard = echo_agent_test_support::env::EnvGuard::set("ECHO_AGENT_HOME", home.path());

    let answers = || {
        let root = crate::config::load_from_disk().expect("read config");
        super::super::load_config_from_toml(&root).consent.answers
    };

    set_consent_answer(Some("a@example.com".into()), "tos".into(), 3, false)
        .await
        .expect("first answer");
    set_consent_answer(Some("a@example.com".into()), "tos".into(), 1, false)
        .await
        .expect("replayed answer");
    assert_eq!(
        answers()["tos"].version,
        3,
        "a stale replay must not lower the record",
    );

    set_consent_answer(Some("a@example.com".into()), "tos".into(), 4, true)
        .await
        .expect("server ack");
    assert!(answers()["tos"].acked, "the ack must reach the record");

    set_consent_answer(Some("a@example.com".into()), "tos".into(), 1, false)
        .await
        .expect("replay after the ack");
    let entry = answers()["tos"].clone();
    assert_eq!(entry.version, 4);
    assert!(
        entry.acked,
        "a replay must not unset the ack it did not make"
    );

    // The local write and the server ack race for one version, and the local one carries `false`.
    set_consent_answer(Some("a@example.com".into()), "tos".into(), 4, false)
        .await
        .expect("the slower local write");
    assert!(
        answers()["tos"].acked,
        "the slower writer must not retract the ack"
    );

    set_consent_answer(Some("b@example.com".into()), "tos".into(), 1, false)
        .await
        .expect("second account");
    let entry = answers()["tos"].clone();
    assert_eq!(entry.version, 1, "a different account starts over");
    assert_eq!(entry.account.as_deref(), Some("b@example.com"));
    assert!(
        !entry.acked,
        "the ack belongs to the answer it was made for"
    );

    set_consent_answer(None, "tos".into(), 2, false)
        .await
        .expect("signed-out answer");
    assert_eq!(
        answers()["tos"].account,
        None,
        "a signed-out answer must not read back as the previous account",
    );

    set_consent_answer(Some("b@example.com".into()), "aup".into(), 2, false)
        .await
        .expect("second notice");
    assert_eq!(
        answers().len(),
        2,
        "answering a second notice must not evict the first",
    );
}
