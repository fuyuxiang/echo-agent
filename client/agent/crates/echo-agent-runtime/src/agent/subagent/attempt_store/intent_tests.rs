use super::{codec::*, intent::*, rewind::RewindMutationId};

const U: u64 = u64::MAX;
const T: u64 = 9_999_999_999_999;
fn h<K>(byte: u8) -> DomainBytes<32, K> {
    DomainBytes::new([byte; 32])
}
fn proof<K>(operation: AttemptTransactionOperationV1) -> RewindIntentProof<K> {
    RewindIntentProof {
        operation,
        temp: AttemptTransactionTempV1::None,
        mutation: RewindMutationId::new([1; 16]),
        subject_hash: h(2),
        base_file_len: ChildRewindFileLen::new(U),
        base_file_hash: h(3),
        result_file_len: ChildRewindFileLen::new(U),
        result_file_hash: h(4),
        base_authority_hash: h(5),
        result_authority_hash: h(6),
        base_attempt_rows_hash: h(7),
        result_attempt_rows_hash: h(8),
        ledger_sequence: MutationLedgerSequence::new(U),
        commit_row_hash: h(9),
        base_checkpoint_generation: RewindCheckpointGeneration::new(U - 1),
        base_checkpoint_root: h(10),
        result_checkpoint_generation: RewindCheckpointGeneration::new(U),
        result_checkpoint_root: h(11),
    }
}
fn intents(phase: AttemptTransactionPhaseV1) -> [AttemptTransactionIntentV1; 5] {
    [
        AttemptTransactionIntentV1::RegisterRewindRef(RegisterRewindRefIntent {
            phase,
            proof: proof(AttemptTransactionOperationV1::RegisterRewindRef),
            generation: SegmentGeneration::try_new(33).unwrap(),
            row_len: CanonicalRewindRowLen::new(U),
            timestamp: Timestamp::try_new(T).unwrap(),
        }),
        AttemptTransactionIntentV1::SupersedeRewindRefs(SupersedeRewindRefsIntent {
            phase,
            proof: proof(AttemptTransactionOperationV1::SupersedeMerge),
            timestamp: Timestamp::try_new(T).unwrap(),
        }),
        AttemptTransactionIntentV1::ReleaseRewindRefs(ReleaseRewindRefsIntent {
            phase,
            proof: proof(AttemptTransactionOperationV1::ReleaseRewindRefs),
            timestamp: Timestamp::try_new(T).unwrap(),
        }),
        AttemptTransactionIntentV1::RelocationHandoff(RelocationHandoffIntent {
            phase,
            proof: proof(AttemptTransactionOperationV1::RelocationHandoff),
            locator_revision: LocatorRevision::new(U),
            timestamp: Timestamp::try_new(T).unwrap(),
        }),
        AttemptTransactionIntentV1::CompactAgentInputs(CompactAgentInputsIntent {
            phase,
            operation: AttemptTransactionOperationV1::CompactAgentInputs,
            temp: AttemptTransactionTempV1::CompactAgentInputs,
            mutation: RewindMutationId::new([1; 16]),
            base_len: AttemptJournalLen::new(U),
            base_hash: h(3),
            result_len: AttemptJournalLen::new(U),
            result_hash: h(4),
            checkpoint_generation: JournalCheckpointGeneration::new(U),
            timestamp: Timestamp::try_new(T).unwrap(),
        }),
    ]
}
fn encoded(intent: &AttemptTransactionIntentV1) -> Vec<u8> {
    EncodedAttemptTransactionIntent::try_new(intent)
        .unwrap()
        .as_bytes()
        .to_vec()
}
fn replace(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    let start = bytes.windows(old.len()).position(|v| v == old).unwrap();
    [&bytes[..start], new, &bytes[start + old.len()..]].concat()
}

#[test]
fn remaining_intents_exact_goldens_and_complete_metadata() {
    let id = "01".repeat(16);
    let hashes = (2..=11)
        .map(|value| format!("{value:02x}").repeat(32))
        .collect::<Vec<_>>();
    let common = |tag, operation, locator: Option<u64>| {
        let locator = locator.map_or_else(String::new, |value| format!(",\"lr\":{value}"));
        format!(
            r#"{{"v":1,"k":{tag},"p":3,"o":{operation},"s":0,"m":"{id}","x":"{}","bl":{U},"bh":"{}","rl":{U},"rh":"{}","ba":"{}","ra":"{}","bj":"{}","rj":"{}","q":{U},"c":"{}","bg":{},"br":"{}","rg":{U},"rr":"{}"{locator},"t":{T}}}
"#,
            hashes[0],
            hashes[1],
            hashes[2],
            hashes[3],
            hashes[4],
            hashes[5],
            hashes[6],
            hashes[7],
            U - 1,
            hashes[8],
            hashes[9],
        )
    };
    let expected = [
        format!(
            r#"{{"v":1,"k":0,"p":3,"o":0,"s":0,"m":"{id}","g":33,"x":"{}","z":{U},"bl":{U},"bh":"{}","rl":{U},"rh":"{}","ba":"{}","ra":"{}","bj":"{}","rj":"{}","q":{U},"c":"{}","bg":{},"br":"{}","rg":{U},"rr":"{}","t":{T}}}
"#,
            hashes[0],
            hashes[1],
            hashes[2],
            hashes[3],
            hashes[4],
            hashes[5],
            hashes[6],
            hashes[7],
            U - 1,
            hashes[8],
            hashes[9],
        ),
        common(1, 2, None),
        common(2, 3, None),
        common(3, 4, Some(U)),
        format!(
            r#"{{"v":1,"k":4,"p":3,"o":5,"s":1,"m":"{id}","bl":{U},"bh":"{}","rl":{U},"rh":"{}","g":{U},"t":{T}}}
"#,
            hashes[1], hashes[2]
        ),
    ];
    let maximum = intents(AttemptTransactionPhaseV1::ProjectionsCommitted);
    let maxima = [
        REGISTER_REWIND_REF_EXACT_BYTES,
        SUPERSEDE_REWIND_REFS_EXACT_BYTES,
        RELEASE_REWIND_REFS_EXACT_BYTES,
        RELOCATION_HANDOFF_EXACT_BYTES,
        COMPACT_AGENT_INPUTS_EXACT_BYTES,
    ];
    for ((intent, expected), max) in maximum.iter().zip(expected).zip(maxima) {
        let bytes = encoded(intent);
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(bytes.len(), max);
        assert_eq!(decode_attempt_transaction_intent(&bytes).unwrap(), *intent);
    }
    assert_eq!(
        INTENT_BYTES,
        [
            (968, 1_024),
            (936, 1_024),
            (936, 1_024),
            (962, 1_024),
            (310, 1_024)
        ]
    );
    assert_eq!(INTENT_PAIR_EXACT_BYTES, 1_936);
    assert_eq!(INTENT_PAIR_ALIGNED_BYTES, 2_048);
}

#[test]
fn complete_family_all_phases_and_supersede_operations_roundtrip() {
    for phase in [
        AttemptTransactionPhaseV1::Prepared,
        AttemptTransactionPhaseV1::SubordinatePrepared,
        AttemptTransactionPhaseV1::CommitObserved,
        AttemptTransactionPhaseV1::ProjectionsCommitted,
    ] {
        for intent in intents(phase) {
            assert_eq!(
                decode_attempt_transaction_intent(&encoded(&intent)).unwrap(),
                intent
            );
        }
    }
    let mut supersede = intents(AttemptTransactionPhaseV1::Prepared)[1].clone();
    let AttemptTransactionIntentV1::SupersedeRewindRefs(value) = &mut supersede else {
        unreachable!("fixture is supersede")
    };
    value.proof.operation = AttemptTransactionOperationV1::SupersedeTruncate;
    assert_eq!(
        decode_attempt_transaction_intent(&encoded(&supersede)).unwrap(),
        supersede
    );
}

#[test]
fn remaining_intents_reject_noncanonical_and_illegal_products() {
    let records = intents(AttemptTransactionPhaseV1::Prepared);
    let supersede = encoded(&records[1]);
    let release = encoded(&records[2]);
    let compact = encoded(&records[4]);
    let invalid = [
        supersede[..supersede.len() - 1].to_vec(),
        [supersede.as_slice(), b"\n"].concat(),
        replace(&supersede, b"\"p\":0,\"o\":2", b"\"o\":2,\"p\":0"),
        replace(&supersede, b"\"p\":0", b"\"p\":4"),
        replace(&supersede, b"\"o\":2", b"\"o\":3"),
        replace(&supersede, b"\"s\":0", b"\"s\":1"),
        replace(&supersede, b"\"x\":\"02", b"\"x\":\"A2"),
        replace(&supersede, b"\"rg\":18446744073709551615", b"\"rg\":0"),
        replace(&release, b"\"o\":3", b"\"o\":2"),
        replace(&compact, b"\"s\":1", b"\"s\":0"),
        replace(&compact, b"\"o\":5", b"\"o\":4"),
        replace(
            &compact,
            b"\"t\":9999999999999",
            b"\"z\":0,\"t\":9999999999999",
        ),
        replace(&compact, b"\"k\":4", b"\"k\":5"),
        vec![b' '; INTENT_ALIGNED_BYTES + 1],
    ];
    for (case, bytes) in invalid.iter().enumerate() {
        assert!(
            decode_attempt_transaction_intent(bytes).is_err(),
            "case {case}"
        );
    }
    for (mut bytes, max) in [
        (supersede, SUPERSEDE_REWIND_REFS_EXACT_BYTES),
        (compact, COMPACT_AGENT_INPUTS_EXACT_BYTES),
    ] {
        bytes.insert(bytes.len() - 1, b' ');
        assert_eq!(
            decode_attempt_transaction_intent(&bytes),
            Err(CodecError::Limit {
                field: "encoded intent",
                actual: max + 1,
                max,
            })
        );
    }
    assert!(AttemptTransactionTagV1::try_from_ordinal(5).is_err());
    assert!(AttemptTransactionOperationV1::try_from_ordinal(6).is_err());
    assert!(AttemptTransactionTempV1::try_from_ordinal(2).is_err());
}
