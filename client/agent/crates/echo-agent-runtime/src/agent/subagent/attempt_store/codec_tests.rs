use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::codec::*;

const G: u64 = u64::MAX;
const T: u64 = 9_999_999_999_999;
macro_rules! d {
    ($t:ty,$b:expr) => {
        <$t>::new([$b; 16])
    };
}
macro_rules! h {
    ($t:ty,$b:expr) => {
        <$t>::new([$b; 32])
    };
}
fn sg(value: u8) -> SegmentGeneration {
    SegmentGeneration::try_new(value).unwrap()
}
fn ts() -> Timestamp {
    Timestamp::try_new(T).unwrap()
}
fn hx(byte: u8, width: usize) -> String {
    format!("{byte:02x}").repeat(width)
}
pub(super) fn text(bytes: &[u8]) -> AgentText {
    AgentText::try_new(bytes).unwrap()
}
fn line(body: String) -> String {
    body + "\n"
}

pub(super) fn records(content: AgentText) -> Vec<RecordV1> {
    vec![
        RecordV1::AttemptHeader(AttemptHeaderRecord {
            attempt: d!(AttemptId, 1),
            owner_root: d!(OwnershipRootId, 2),
            generation: AttemptGeneration::try_new(G).unwrap(),
            initial: SegmentKindV1::InitialAgentMessage,
            constructor_digest: h!(ConstructorDigest, 3),
            timestamp: ts(),
        }),
        RecordV1::CapacityReserved(CapacityReservedRecord {
            profile: CapacityProfile {
                segments: 33,
                messages: 32,
                raw_bytes: 262_144,
                content_bytes: 393_216,
                logical_bytes: 524_288,
                physical_bytes: 1_048_576,
                directory_bytes: 786_432,
            },
            timestamp: ts(),
        }),
        RecordV1::AcceptedAgentContent(AcceptedAgentContentRecord {
            generation: sg(1),
            kind: SegmentKindV1::InitialAgentMessage,
            message: d!(AgentMessageId, 4),
            sender_session: d!(SenderSessionId, 5),
            source_attempt: Some(d!(SourceAttemptId, 6)),
            relation: AgentSenderRelationV1::ParentToOwnedDescendant,
            authority: AgentAuthorityV1::ModelAuthoredUntrusted,
            text: content,
            timestamp: ts(),
        }),
        RecordV1::SegmentReserved(SegmentReservedRecord {
            generation: sg(33),
            barrier: BarrierGeneration::new(G),
            kind: SegmentKindV1::AttachedHuman,
            prompt: d!(PromptId, 7),
            payload_hash: h!(PayloadHash, 8),
            timestamp: ts(),
        }),
        RecordV1::TurnStarted(TurnStartedRecord {
            generation: sg(33),
            timestamp: ts(),
        }),
        RecordV1::TurnCommitIntent(TurnCommitIntentRecord {
            generation: sg(33),
            projection_hash: h!(ProjectionSetHash, 9),
            core_hash: h!(CompletionCoreHash, 10),
            timestamp: ts(),
        }),
        RecordV1::TurnResolution(TurnResolutionRecord {
            generation: sg(33),
            resolution: TurnResolutionV1::Delivered,
            core_hash: h!(CompletionCoreHash, 11),
            rewind_ref_hash: Some(h!(RewindRefHash, 12)),
            timestamp: ts(),
        }),
        RecordV1::InputDisposition(InputDispositionRecord {
            generation: sg(33),
            disposition: InputDispositionV1::Cancelled,
            timestamp: ts(),
        }),
        RecordV1::QueueCut(QueueCutRecord {
            generation: sg(33),
            queue_generation: QueueGeneration::new(G),
            timestamp: ts(),
        }),
        RecordV1::AdmissionClosed(AdmissionClosedRecord {
            barrier: BarrierGeneration::new(G),
            reason: AdmissionCloseReasonV1::Corruption,
            timestamp: ts(),
        }),
        RecordV1::AttemptOutcome(AttemptOutcomeRecord {
            high_water: SegmentHighWater::try_new(33).unwrap(),
            outcome: AttemptOutcomeV1::Quarantined,
            timestamp: ts(),
        }),
    ]
}

#[test]
fn exact_goldens_and_limits() {
    let x = |byte, width| hx(byte, width);
    let expected = vec![
        line(format!(
            r#"{{"v":1,"e":0,"a":"{}","r":"{}","g":{G},"k":1,"h":"{}","t":{T}}}"#,
            x(1, 16),
            x(2, 16),
            x(3, 32)
        )),
        line(format!(
            r#"{{"v":1,"e":1,"s":33,"m":32,"b":262144,"c":393216,"l":524288,"p":1048576,"d":786432,"t":{T}}}"#
        )),
        line(format!(
            r#"{{"v":1,"e":2,"g":1,"k":1,"m":"{}","s":"{}","x":"{}","r":0,"a":0,"c":"","t":{T}}}"#,
            x(4, 16),
            x(5, 16),
            x(6, 16)
        )),
        line(format!(
            r#"{{"v":1,"e":3,"g":33,"b":{G},"k":3,"p":"{}","h":"{}","t":{T}}}"#,
            x(7, 16),
            x(8, 32)
        )),
        line(format!(r#"{{"v":1,"e":4,"g":33,"t":{T}}}"#)),
        line(format!(
            r#"{{"v":1,"e":5,"g":33,"p":"{}","c":"{}","t":{T}}}"#,
            x(9, 32),
            x(10, 32)
        )),
        line(format!(
            r#"{{"v":1,"e":6,"g":33,"o":0,"c":"{}","r":"{}","t":{T}}}"#,
            x(11, 32),
            x(12, 32)
        )),
        line(format!(r#"{{"v":1,"e":7,"g":33,"o":3,"r":2,"t":{T}}}"#)),
        line(format!(r#"{{"v":1,"e":8,"g":33,"q":{G},"t":{T}}}"#)),
        line(format!(r#"{{"v":1,"e":9,"b":{G},"o":3,"t":{T}}}"#)),
        line(format!(r#"{{"v":1,"e":10,"w":33,"o":3,"r":3,"t":{T}}}"#)),
    ];
    let records = records(text(b""));
    assert_eq!((records.len(), expected.len()), (11, 11));
    assert_eq!(
        records.iter().map(RecordV1::limits).collect::<Vec<_>>(),
        vec![
            (212, 224),
            (102, 128),
            (43_871, 49_152),
            (180, 192),
            (39, 64),
            (181, 192),
            (187, 192),
            (51, 64),
            (64, 64),
            (63, 64),
            (52, 64)
        ]
    );
    for (record, expected) in records.iter().zip(expected) {
        let actual = EncodedRecord::try_new(record).unwrap();
        assert_eq!(actual.as_bytes(), expected.as_bytes());
    }
}

#[test]
fn max_content_is_exact_and_unpadded() {
    let boundary = records(text(&vec![b'a'; MAX_MESSAGE_RAW_BYTES])).remove(2);
    let encoded = EncodedRecord::try_new(&boundary).unwrap();
    assert_eq!(encoded.len(), 43_871);
    let encoded_text = URL_SAFE_NO_PAD.encode(vec![b'a'; MAX_MESSAGE_RAW_BYTES]);
    assert!(
        std::str::from_utf8(encoded.as_bytes())
            .unwrap()
            .contains(&format!(r#""c":"{encoded_text}""#))
    );
    assert!(!encoded_text.contains('='));
    assert!(AgentText::try_new(&vec![b'a'; MAX_MESSAGE_RAW_BYTES + 1]).is_err());
    assert!(AgentText::try_new(&[0xff]).is_err());
}

#[test]
fn typed_scalars_and_constructor_semantics_are_closed() {
    assert_eq!(sg(1).index(), 0);
    assert_eq!(sg(33).index(), 32);
    for value in [0, 34] {
        assert!(SegmentGeneration::try_new(value).is_err());
    }
    assert!(AttemptGeneration::try_new(0).is_err());
    assert!(SegmentHighWater::try_new(34).is_err());
    let no_source = RecordV1::AcceptedAgentContent(AcceptedAgentContentRecord {
        generation: sg(1),
        kind: SegmentKindV1::InitialAgentMessage,
        message: d!(AgentMessageId, 1),
        sender_session: d!(SenderSessionId, 2),
        source_attempt: None,
        relation: AgentSenderRelationV1::ParentToOwnedDescendant,
        authority: AgentAuthorityV1::ModelAuthoredUntrusted,
        text: text(b""),
        timestamp: ts(),
    });
    let later_source = RecordV1::AcceptedAgentContent(AcceptedAgentContentRecord {
        generation: sg(2),
        kind: SegmentKindV1::AgentMessage,
        message: d!(AgentMessageId, 1),
        sender_session: d!(SenderSessionId, 2),
        source_attempt: Some(d!(SourceAttemptId, 3)),
        relation: AgentSenderRelationV1::ParentToOwnedDescendant,
        authority: AgentAuthorityV1::ModelAuthoredUntrusted,
        text: text(b""),
        timestamp: ts(),
    });
    let failed_ref = RecordV1::TurnResolution(TurnResolutionRecord {
        generation: sg(1),
        resolution: TurnResolutionV1::Failed,
        core_hash: h!(CompletionCoreHash, 1),
        rewind_ref_hash: Some(h!(RewindRefHash, 2)),
        timestamp: ts(),
    });
    for bad in [&no_source, &later_source, &failed_ref] {
        assert!(EncodedRecord::try_new(bad).is_err());
    }
}

#[test]
fn capacity_bounds_and_legal_products() {
    let bad = |field: &str| CapacityReservedRecord {
        profile: CapacityProfile {
            segments: if field == "s" { 34 } else { 33 },
            messages: if field == "m" { 33 } else { 32 },
            raw_bytes: if field == "b" { 262_145 } else { 262_144 },
            content_bytes: if field == "c" { 393_217 } else { 393_216 },
            logical_bytes: if field == "l" { 524_289 } else { 524_288 },
            physical_bytes: if field == "p" { 1_048_577 } else { 1_048_576 },
            directory_bytes: if field == "d" { 786_433 } else { 786_432 },
        },
        timestamp: ts(),
    };
    for field in ["s", "m", "b", "c", "l", "p", "d"] {
        assert!(
            EncodedRecord::try_new(&RecordV1::CapacityReserved(bad(field))).is_err(),
            "{field}"
        );
    }
    for (value, expected) in [
        (
            InputDispositionV1::Queued,
            line(r#"{"v":1,"e":7,"g":1,"o":0,"r":0,"t":0}"#.into()),
        ),
        (
            InputDispositionV1::Delivered,
            line(r#"{"v":1,"e":7,"g":1,"o":1,"r":0,"t":0}"#.into()),
        ),
        (
            InputDispositionV1::Failed,
            line(r#"{"v":1,"e":7,"g":1,"o":2,"r":1,"t":0}"#.into()),
        ),
        (
            InputDispositionV1::Cancelled,
            line(r#"{"v":1,"e":7,"g":1,"o":3,"r":2,"t":0}"#.into()),
        ),
    ] {
        let record = RecordV1::InputDisposition(InputDispositionRecord {
            generation: sg(1),
            disposition: value,
            timestamp: Timestamp::try_new(0).unwrap(),
        });
        assert_eq!(
            EncodedRecord::try_new(&record).unwrap().as_bytes(),
            expected.as_bytes()
        );
    }
    for (value, expected) in [
        (
            AttemptOutcomeV1::Completed,
            line(r#"{"v":1,"e":10,"w":0,"o":0,"r":0,"t":0}"#.into()),
        ),
        (
            AttemptOutcomeV1::Failed,
            line(r#"{"v":1,"e":10,"w":0,"o":1,"r":1,"t":0}"#.into()),
        ),
        (
            AttemptOutcomeV1::Cancelled,
            line(r#"{"v":1,"e":10,"w":0,"o":2,"r":2,"t":0}"#.into()),
        ),
        (
            AttemptOutcomeV1::Quarantined,
            line(r#"{"v":1,"e":10,"w":0,"o":3,"r":3,"t":0}"#.into()),
        ),
    ] {
        let record = RecordV1::AttemptOutcome(AttemptOutcomeRecord {
            high_water: SegmentHighWater::try_new(0).unwrap(),
            outcome: value,
            timestamp: Timestamp::try_new(0).unwrap(),
        });
        assert_eq!(
            EncodedRecord::try_new(&record).unwrap().as_bytes(),
            expected.as_bytes()
        );
    }
}
