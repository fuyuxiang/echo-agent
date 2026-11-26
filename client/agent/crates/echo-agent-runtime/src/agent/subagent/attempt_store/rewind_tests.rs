use super::{
    codec::*,
    decoder::{DecodedAttemptRecordV1, decode_attempt_record, journal_prefix_hash},
    rewind::*,
};

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
fn sg() -> SegmentGeneration {
    SegmentGeneration::try_new(33).unwrap()
}
fn ts() -> Timestamp {
    Timestamp::try_new(T).unwrap()
}
fn hx(byte: u8, width: usize) -> String {
    format!("{byte:02x}").repeat(width)
}
fn line(body: String) -> String {
    body + "\n"
}

fn records() -> Vec<RewindRecordV1> {
    vec![
        RewindRecordV1::Live(RewindRefLiveRecord {
            generation: sg(),
            child_storage: d!(ChildStorageKey, 1),
            prompt_index: PromptIndex::new(G),
            serialized_row_len: SerializedRowLen::new(G),
            row_hash: h!(RewindRowHash, 2),
            timestamp: ts(),
        }),
        RewindRecordV1::Superseded(RewindRefSupersededRecord {
            generation: sg(),
            removed_ref_hash: h!(RemovedRefHash, 3),
            removing_mutation: d!(RewindMutationId, 4),
            pre_removal_ancestry: h!(PreRemovalAncestryRoot, 5),
            resulting_file_hash: h!(CanonicalRewindFileHash, 6),
            timestamp: ts(),
        }),
        RewindRecordV1::ReleaseIntent(RewindRefReleaseRecord {
            generation: ReleaseGeneration::new(G),
            authority_set_hash: h!(AuthoritySetHash, 7),
            timestamp: ts(),
        }),
        RewindRecordV1::ReleaseReceipt(RewindRefReleaseRecord {
            generation: ReleaseGeneration::new(G),
            authority_set_hash: h!(AuthoritySetHash, 7),
            timestamp: ts(),
        }),
        RewindRecordV1::Checkpoint(JournalCheckpointRecord {
            generation: CheckpointGeneration::new(G),
            prefix_len: JournalPrefixLen::new(G),
            prefix_hash: h!(JournalPrefixHash, 8),
            accepted_rows: AcceptedRowCount::try_new(32).unwrap(),
            control_rows: ControlRowCount::try_new(238).unwrap(),
            supersession_rows: SupersessionRowCount::try_new(33).unwrap(),
            timestamp: ts(),
        }),
    ]
}

#[test]
fn exact_goldens_limits_and_roundtrips() {
    let x = |byte, width| hx(byte, width);
    let expected = [
        line(format!(
            r#"{{"v":1,"e":11,"g":33,"s":"{}","p":{G},"l":{G},"w":"{}","t":{T}}}"#,
            x(1, 16),
            x(2, 32)
        )),
        line(format!(
            r#"{{"v":1,"e":12,"g":33,"h":"{}","m":"{}","a":"{}","f":"{}","t":{T}}}"#,
            x(3, 32),
            x(4, 16),
            x(5, 32),
            x(6, 32)
        )),
        line(format!(
            r#"{{"v":1,"e":13,"g":{G},"h":"{}","t":{T}}}"#,
            x(7, 32)
        )),
        line(format!(
            r#"{{"v":1,"e":14,"g":{G},"h":"{}","t":{T}}}"#,
            x(7, 32)
        )),
        line(format!(
            r#"{{"v":1,"e":15,"q":{G},"l":{G},"h":"{}","a":32,"c":238,"s":33,"t":{T}}}"#,
            x(8, 32)
        )),
    ];
    let records = records();
    assert_eq!(
        records
            .iter()
            .map(RewindRecordV1::limits)
            .collect::<Vec<_>>(),
        A2_EXACT_ALIGNED_ROW_BYTES
    );
    let core = b"{\"v\":1,\"e\":4,\"g\":1,\"t\":0}\n";
    assert!(matches!(
        decode_attempt_record(core).unwrap(),
        DecodedAttemptRecordV1::Core(_)
    ));
    for (record, expected) in records.iter().zip(expected) {
        let encoded = EncodedRewindRecord::try_new(record).unwrap();
        assert_eq!(encoded.as_bytes(), expected.as_bytes());
        assert_eq!(
            decode_attempt_record(encoded.as_bytes()).unwrap(),
            DecodedAttemptRecordV1::Rewind(record.clone())
        );
    }
}

fn replace(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    let start = bytes
        .windows(old.len())
        .position(|window| window == old)
        .unwrap();
    let mut changed = Vec::with_capacity(bytes.len() - old.len() + new.len());
    changed.extend_from_slice(&bytes[..start]);
    changed.extend_from_slice(new);
    changed.extend_from_slice(&bytes[start + old.len()..]);
    changed
}

#[test]
fn strict_decoder_rejects_labeled_adversarial_inputs() {
    let encoded = records()
        .iter()
        .map(|record| {
            EncodedRewindRecord::try_new(record)
                .unwrap()
                .as_bytes()
                .to_vec()
        })
        .collect::<Vec<_>>();
    let reordered_live = line(format!(
        r#"{{"v":1,"e":11,"g":33,"p":{G},"s":"{}","l":{G},"w":"{}","t":{T}}}"#,
        hx(1, 16),
        hx(2, 32)
    ))
    .into_bytes();
    let adversarial = [
        (
            "unknown event",
            b"{\"v\":1,\"e\":16,\"g\":1,\"t\":0}\n".to_vec(),
        ),
        (
            "zero segment generation",
            replace(&encoded[0], b"\"g\":33", b"\"g\":0"),
        ),
        (
            "uppercase storage key",
            replace(&encoded[0], b"\"s\":\"01", b"\"s\":\"A1"),
        ),
        (
            "wide mutation ID",
            replace(&encoded[1], b"\"m\":\"0404", b"\"m\":\"040404"),
        ),
        (
            "duplicate generation",
            replace(
                &encoded[2],
                b"\"g\":18446744073709551615",
                b"\"g\":18446744073709551615,\"g\":1",
            ),
        ),
        (
            "accepted count overflow",
            replace(&encoded[4], b"\"a\":32", b"\"a\":33"),
        ),
        (
            "control count overflow",
            replace(&encoded[4], b"\"c\":238", b"\"c\":239"),
        ),
        (
            "u64 overflow",
            replace(
                &encoded[4],
                b"\"q\":18446744073709551615",
                b"\"q\":18446744073709551616",
            ),
        ),
        (
            "unknown key",
            replace(&encoded[4], b",\"s\":33", b",\"z\":0,\"s\":33"),
        ),
        ("complete reordered row", reordered_live),
        ("missing LF", encoded[0][..encoded[0].len() - 1].to_vec()),
        ("extra LF", [encoded[0].as_slice(), b"\n"].concat()),
        ("global cap", vec![b' '; MAX_ENCODED_RECORD_BYTES + 1]),
    ];
    for (case, bytes) in adversarial {
        assert!(decode_attempt_record(&bytes).is_err(), "{case}");
    }
}

#[test]
fn variant_cap_precedes_json_parse_below_global_cap() {
    let row = line(format!(
        r#"{{"v":1,"e":13,"g":{G},"h":"{}","t":{T},"z":"{}"}}"#,
        hx(7, 32),
        "a".repeat(25)
    ));
    assert_eq!(row.len(), 161);
    assert!(row.len() < MAX_ENCODED_RECORD_BYTES);
    assert_eq!(
        decode_attempt_record(row.as_bytes()),
        Err(CodecError::Limit {
            field: "encoded record",
            actual: 161,
            max: 160,
        })
    );
}

#[test]
fn hashes_pin_exact_lf_row_and_mixed_prefix_bytes() {
    let rewind_row = b"{}\n";
    assert_eq!(
        rewind_row_hash(rewind_row).unwrap().as_bytes(),
        &[
            0xca, 0x3d, 0x16, 0x3b, 0xab, 0x05, 0x53, 0x81, 0x82, 0x72, 0x26, 0x14, 0x05, 0x68,
            0xf3, 0xbe, 0xf7, 0xea, 0xac, 0x18, 0x7c, 0xeb, 0xd7, 0x68, 0x78, 0xe0, 0xb6, 0x3e,
            0x9e, 0x44, 0x23, 0x56
        ]
    );
    for bytes in [b"{}".as_slice(), b"{}\r\n", b"{}\n{}"] {
        assert!(rewind_row_hash(bytes).is_err());
    }

    let core = b"{\"v\":1,\"e\":4,\"g\":1,\"t\":0}\n";
    let live = EncodedRewindRecord::try_new(&records()[0]).unwrap();
    let prefix = [core.as_slice(), live.as_bytes()].concat();
    assert_eq!(
        journal_prefix_hash(&prefix).unwrap().as_bytes(),
        &[
            0xd5, 0x87, 0x11, 0x9a, 0xd5, 0x7c, 0xda, 0x31, 0xd2, 0xb5, 0x7b, 0x34, 0x6d, 0x72,
            0x6a, 0x01, 0xf1, 0x87, 0x30, 0xc0, 0xad, 0x50, 0x6b, 0xc3, 0x13, 0xa2, 0x17, 0xd0,
            0x87, 0x08, 0x24, 0x77
        ]
    );
    assert!(journal_prefix_hash(&prefix[..prefix.len() - 1]).is_err());
    assert!(journal_prefix_hash(b"{}\n").is_err());
}
