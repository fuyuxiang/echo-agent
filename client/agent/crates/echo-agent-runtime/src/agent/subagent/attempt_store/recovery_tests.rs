use super::{codec::*, recovery::*};

const T: u64 = 9_999_999_999_999;
fn sg(value: u8) -> SegmentGeneration {
    SegmentGeneration::try_new(value).unwrap()
}
fn generation(value: u8) -> RecoveryGenerationV1 {
    RecoveryGenerationV1::Known(sg(value))
}
fn unknown_generation() -> RecoveryGenerationV1 {
    RecoveryGenerationV1::Unknown
}
fn key(generation: RecoveryGenerationV1, run: u8) -> RecoveryRunKeyV1 {
    RecoveryRunKeyV1::try_new(generation, RecoveryRunV1::try_new(run).unwrap()).unwrap()
}
fn ts() -> Timestamp {
    Timestamp::try_new(T).unwrap()
}
fn payload_hash(byte: u8) -> RecoveryOutcomePayloadHash {
    RecoveryOutcomePayloadHash::new([byte; 32])
}
fn hx(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}
fn replace(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    let start = bytes
        .windows(old.len())
        .position(|value| value == old)
        .unwrap();
    [&bytes[..start], new, &bytes[start + old.len()..]].concat()
}
fn with_generation(bytes: &[u8], generation: u64) -> Vec<u8> {
    replace(
        bytes,
        b"18446744073709551615",
        generation.to_string().as_bytes(),
    )
}
fn outcome(
    generation: RecoveryGenerationV1,
    run: u8,
    resolution: TurnResolutionV1,
    outcome: RecoveryOutcomeV1,
    retry_bucket: RecoveryRetryBucketV1,
) -> RecoveryRecordV1 {
    RecoveryRecordV1::Outcome(RecoveryOutcome {
        key: key(generation, run),
        payload_hash: payload_hash(4),
        resolution,
        outcome,
        retry_bucket,
        timestamp: ts(),
    })
}
fn terminal_records() -> Vec<RecoveryRecordV1> {
    let generation = unknown_generation();
    let key = key(generation, 7);
    vec![
        RecoveryRecordV1::RunReserved(RecoveryRunReserved {
            key,
            timestamp: ts(),
        }),
        outcome(
            generation,
            7,
            TurnResolutionV1::Cancelled,
            RecoveryOutcomeV1::Quarantined,
            RecoveryRetryBucketV1::EightPlus,
        ),
        RecoveryRecordV1::Claim(RecoveryClaim {
            key,
            payload_hash: payload_hash(4),
            timestamp: ts(),
        }),
    ]
}
fn encoded(record: &RecoveryRecordV1) -> Vec<u8> {
    EncodedRecoveryRecord::try_new(record)
        .unwrap()
        .as_bytes()
        .to_vec()
}

#[test]
fn exact_lf_goldens_lengths_limits_and_roundtrips() {
    let expected = [
        format!(
            r#"{{"v":1,"e":0,"g":18446744073709551615,"r":7,"t":{T}}}
"#
        ),
        format!(
            r#"{{"v":1,"e":1,"g":18446744073709551615,"r":7,"h":"{}","x":2,"o":2,"b":7,"t":{T}}}
"#,
            hx(4)
        ),
        format!(
            r#"{{"v":1,"e":2,"g":18446744073709551615,"r":7,"h":"{}","t":{T}}}
"#,
            hx(4)
        ),
    ];
    let records = terminal_records();
    assert_eq!(
        records
            .iter()
            .map(RecoveryRecordV1::limits)
            .collect::<Vec<_>>(),
        RECOVERY_ROW_BYTES
    );
    for ((record, expected), metadata) in records.iter().zip(expected).zip(RECOVERY_ROW_BYTES) {
        let bytes = encoded(record);
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(bytes.len(), metadata.0);
        assert_eq!(decode_recovery_record(&bytes).unwrap(), *record);
    }
}

#[test]
fn closed_resolutions_outcomes_and_retry_buckets_roundtrip() {
    let generation = generation(1);
    for resolution in [
        TurnResolutionV1::Delivered,
        TurnResolutionV1::Failed,
        TurnResolutionV1::Cancelled,
    ] {
        let record = outcome(
            generation,
            0,
            resolution,
            RecoveryOutcomeV1::Recovered,
            RecoveryRetryBucketV1::Zero,
        );
        assert_eq!(decode_recovery_record(&encoded(&record)).unwrap(), record);
    }
    assert!(TurnResolutionV1::try_from_ordinal(3).is_err());
    for ordinal in 0..RecoveryOutcomeV1::COUNT {
        let value = RecoveryOutcomeV1::try_from_ordinal(ordinal).unwrap();
        let bucket = if matches!(value, RecoveryOutcomeV1::Quarantined) {
            RecoveryRetryBucketV1::One
        } else {
            RecoveryRetryBucketV1::Zero
        };
        let record = outcome(generation, 0, TurnResolutionV1::Failed, value, bucket);
        assert_eq!(decode_recovery_record(&encoded(&record)).unwrap(), record);
    }
    assert!(RecoveryOutcomeV1::try_from_ordinal(RecoveryOutcomeV1::COUNT).is_err());
    for ordinal in 0..RecoveryRetryBucketV1::COUNT - 1 {
        let bucket = RecoveryRetryBucketV1::try_from_ordinal(ordinal).unwrap();
        let record = outcome(
            generation,
            0,
            TurnResolutionV1::Failed,
            RecoveryOutcomeV1::CoreFailed,
            bucket,
        );
        assert_eq!(decode_recovery_record(&encoded(&record)).unwrap(), record);
    }
    assert!(RecoveryRetryBucketV1::try_from_ordinal(RecoveryRetryBucketV1::COUNT).is_err());
}

#[test]
fn generation_and_run_key_classes_are_closed() {
    assert_eq!(RecoveryGenerationV1::try_new(33).unwrap(), generation(33));
    for invalid in [0, 34, u64::MAX - 1] {
        assert!(RecoveryGenerationV1::try_new(invalid).is_err());
    }
    assert_eq!(
        RecoveryGenerationV1::try_new(u64::MAX).unwrap(),
        unknown_generation()
    );
    let reserved = encoded(&terminal_records()[0]);
    assert!(decode_recovery_record(&with_generation(&reserved, 33)).is_ok());
    for invalid in [34, u64::MAX - 1] {
        assert!(decode_recovery_record(&with_generation(&reserved, invalid)).is_err());
    }
    assert_eq!(
        decode_recovery_record(&reserved).unwrap(),
        terminal_records()[0]
    );

    for run in 0..=6 {
        assert_eq!(
            key(generation(1), run).classify(),
            RecoveryRunClassV1::OrdinaryMutable
        );
    }
    for generation in [generation(1), unknown_generation()] {
        assert_eq!(
            key(generation, 7).classify(),
            RecoveryRunClassV1::TerminalQuarantine
        );
    }
    assert!(
        RecoveryRunKeyV1::try_new(unknown_generation(), RecoveryRunV1::try_new(0).unwrap())
            .is_err()
    );
    assert!(RecoveryRunV1::try_new(RecoveryRunV1::COUNT).is_err());
}

#[test]
fn ordinary_and_terminal_outcome_rules_are_closed() {
    let known = generation(1);
    let legal_terminal = outcome(
        known,
        7,
        TurnResolutionV1::Cancelled,
        RecoveryOutcomeV1::Quarantined,
        RecoveryRetryBucketV1::EightPlus,
    );
    assert_eq!(
        decode_recovery_record(&encoded(&legal_terminal)).unwrap(),
        legal_terminal
    );

    let invalid = [
        outcome(
            known,
            7,
            TurnResolutionV1::Failed,
            RecoveryOutcomeV1::CoreFailed,
            RecoveryRetryBucketV1::Six,
        ),
        outcome(
            known,
            6,
            TurnResolutionV1::Failed,
            RecoveryOutcomeV1::CoreFailed,
            RecoveryRetryBucketV1::EightPlus,
        ),
        outcome(
            unknown_generation(),
            7,
            TurnResolutionV1::Cancelled,
            RecoveryOutcomeV1::Quarantined,
            RecoveryRetryBucketV1::Six,
        ),
    ];
    for record in invalid {
        assert!(EncodedRecoveryRecord::try_new(&record).is_err());
    }
}

#[test]
fn strict_decoder_rejects_noncanonical_oversized_and_event_cap_rows() {
    let valid = encoded(&terminal_records()[1]);
    let invalid = [
        valid[..valid.len() - 1].to_vec(),
        [valid.as_slice(), b"\n"].concat(),
        replace(
            &valid,
            b"\"g\":18446744073709551615,\"r\":7",
            b"\"r\":7,\"g\":18446744073709551615",
        ),
        replace(&valid, b"\"v\":1", b"\"v\":2"),
        replace(&valid, b"\"e\":1", b"\"e\":2"),
        replace(&valid, b"\"r\":7", b"\"r\":8"),
        replace(&valid, b"\"x\":2", b"\"x\":3"),
        replace(&valid, b"\"o\":2", b"\"o\":3"),
        replace(&valid, b"\"b\":7", b"\"b\":8"),
        replace(&valid, b"\"h\":\"04", b"\"h\":\"A4"),
        replace(
            &valid,
            b"\"t\":9999999999999",
            b"\"z\":0,\"t\":9999999999999",
        ),
        replace(&valid, b"\"x\":2", b"\"x\":2,\"x\":2"),
        vec![b' '; 161],
    ];
    for (case, bytes) in invalid.iter().enumerate() {
        assert!(decode_recovery_record(bytes).is_err(), "case {case}");
    }
    let suffix = b"\"}\n";
    let mut row = b"{\"v\":1,\"e\":0,\"z\":\"".to_vec();
    row.extend(std::iter::repeat_n(b'a', 65 - row.len() - suffix.len()));
    row.extend_from_slice(suffix);
    assert_eq!(
        decode_recovery_record(&row),
        Err(CodecError::Limit {
            field: "encoded recovery record",
            actual: 65,
            max: 64,
        })
    );
}

#[test]
fn exact_accounting_covers_zero_one_decimal_boundary_maximum_and_overflow() {
    let expected = [
        (0, 3, 349),
        (1, 27, 2_685),
        (9, 219, 21_373),
        (10, 243, 23_733),
        (33, 795, 78_013),
    ];
    for (segments, rows, exact_bytes) in expected {
        assert_eq!(
            account_recovery(segments).unwrap(),
            RecoveryAccounting { rows, exact_bytes }
        );
    }
    assert!(account_recovery(34).is_err());
    assert!(account_recovery(u64::MAX).is_err());
}
