use super::{codec::*, completion::*};

const T: u64 = 9_999_999_999_999;
fn sg(value: u8) -> SegmentGeneration {
    SegmentGeneration::try_new(value).unwrap()
}
fn ts() -> Timestamp {
    Timestamp::try_new(T).unwrap()
}
fn effect_hash(byte: u8) -> EffectPayloadHash {
    EffectPayloadHash::new([byte; 32])
}
fn product_hash(byte: u8) -> ProductDescriptorPayloadHash {
    ProductDescriptorPayloadHash::new([byte; 32])
}
fn applied_hash(byte: u8) -> CompletionAppliedSetHash {
    CompletionAppliedSetHash::new([byte; 32])
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
fn progress(effect: CompletionEffectV1, outcome: ProgressOutcomeV1) -> CompletionRecordV1 {
    CompletionRecordV1::Progress(CompletionProgress {
        generation: sg(33),
        effect,
        payload_hash: effect_hash(1),
        outcome,
        observed_revision: u64::MAX,
        timestamp: ts(),
    })
}
fn records() -> Vec<CompletionRecordV1> {
    vec![
        progress(
            CompletionEffectV1::QueueCut,
            ProgressOutcomeV1::AlreadyApplied,
        ),
        CompletionRecordV1::ProductClaim(CompletionProductClaim {
            generation: sg(33),
            slot: ProductClaimSlotV1::try_new(7).unwrap(),
            payload_hash: product_hash(2),
            timestamp: ts(),
        }),
        CompletionRecordV1::Complete(CompletionComplete {
            generation: sg(33),
            applied_set_hash: applied_hash(3),
            timestamp: ts(),
        }),
    ]
}
fn encoded(record: &CompletionRecordV1) -> Vec<u8> {
    EncodedCompletionRecord::try_new(record)
        .unwrap()
        .as_bytes()
        .to_vec()
}

#[test]
fn exact_lf_goldens_lengths_limits_and_roundtrips() {
    let expected = [
        format!(
            r#"{{"v":1,"e":0,"g":33,"i":12,"h":"{}","o":2,"r":18446744073709551615,"t":{T}}}
"#,
            hx(1)
        ),
        format!(
            r#"{{"v":1,"e":1,"g":33,"i":7,"h":"{}","t":{T}}}
"#,
            hx(2)
        ),
        format!(
            r#"{{"v":1,"e":2,"g":33,"h":"{}","t":{T}}}
"#,
            hx(3)
        ),
    ];
    let records = records();
    assert_eq!(
        records
            .iter()
            .map(CompletionRecordV1::limits)
            .collect::<Vec<_>>(),
        COMPLETION_ROW_BYTES
    );
    for ((record, expected), metadata) in records.iter().zip(expected).zip(COMPLETION_ROW_BYTES) {
        let bytes = encoded(record);
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(bytes.len(), metadata.0);
        assert_eq!(decode_completion_record(&bytes).unwrap(), *record);
    }
}

#[test]
fn every_effect_slot_and_outcome_roundtrips_and_first_invalid_rejects() {
    for ordinal in 0..CompletionEffectV1::COUNT {
        let effect = CompletionEffectV1::try_from_ordinal(ordinal).unwrap();
        let record = progress(effect, ProgressOutcomeV1::Applied);
        assert_eq!(decode_completion_record(&encoded(&record)).unwrap(), record);
    }
    assert!(CompletionEffectV1::try_from_ordinal(CompletionEffectV1::COUNT).is_err());
    for ordinal in 0..ProductClaimSlotV1::COUNT {
        let record = CompletionRecordV1::ProductClaim(CompletionProductClaim {
            generation: sg(1),
            slot: ProductClaimSlotV1::try_new(ordinal).unwrap(),
            payload_hash: product_hash(2),
            timestamp: ts(),
        });
        assert_eq!(decode_completion_record(&encoded(&record)).unwrap(), record);
    }
    assert!(ProductClaimSlotV1::try_new(ProductClaimSlotV1::COUNT).is_err());
    for ordinal in 0..ProgressOutcomeV1::COUNT {
        let outcome = ProgressOutcomeV1::try_from_ordinal(ordinal).unwrap();
        let record = progress(CompletionEffectV1::ConversationFence, outcome);
        assert_eq!(decode_completion_record(&encoded(&record)).unwrap(), record);
    }
    assert!(ProgressOutcomeV1::try_from_ordinal(ProgressOutcomeV1::COUNT).is_err());
}

#[test]
fn strict_decoder_rejects_noncanonical_oversized_and_event_cap_rows() {
    let valid = encoded(&records()[0]);
    let invalid = [
        valid[..valid.len() - 1].to_vec(),
        [valid.as_slice(), b"\n"].concat(),
        replace(&valid, b"\"g\":33,\"i\":12", b"\"i\":12,\"g\":33"),
        replace(&valid, b"\"i\":12", b"\"i\":13"),
        replace(&valid, b"\"o\":2", b"\"o\":3"),
        replace(&valid, b"\"h\":\"01", b"\"h\":\"A1"),
        replace(
            &valid,
            b"\"t\":9999999999999",
            b"\"z\":0,\"t\":9999999999999",
        ),
        vec![b' '; 161],
    ];
    for (case, bytes) in invalid.iter().enumerate() {
        assert!(decode_completion_record(bytes).is_err(), "case {case}");
    }
    for event in [1, 2] {
        let suffix = b"\"}\n";
        let prefix = format!(r#"{{"v":1,"e":{event},"z":""#);
        let mut row = prefix.into_bytes();
        row.extend(std::iter::repeat_n(b'a', 129 - row.len() - suffix.len()));
        row.extend_from_slice(suffix);
        assert_eq!(
            decode_completion_record(&row),
            Err(CodecError::Limit {
                field: "encoded completion record",
                actual: 129,
                max: 128,
            })
        );
    }
}

#[test]
fn exact_accounting_covers_decimal_boundary_maximum_and_overflow() {
    // Exact Progress bills one-digit effects (0..=9) one byte under the two-digit-i
    // row max; product/complete rows keep COMPLETION_ROW_BYTES exact widths.
    // Per two-digit generation: 10*147 + 3*148 + 8*116 + 110 = 2_952.
    // Per one-digit generation: 2_952 - 22 = 2_930.
    let expected = [
        (0, 0, 0),
        (1, 22, 2_930),
        (9, 198, 26_370),
        (10, 220, 29_322),
        (33, 726, 97_218),
    ];
    for (segments, rows, exact_bytes) in expected {
        assert_eq!(
            account_completion(segments).unwrap(),
            CompletionAccounting { rows, exact_bytes }
        );
    }
    assert!(account_completion(34).is_err());
    assert!(account_completion(u64::MAX).is_err());
}
