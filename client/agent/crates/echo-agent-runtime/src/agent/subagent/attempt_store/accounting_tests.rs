use super::{accounting::*, codec::*};

fn mix(segments: u8, accepted_row_raw_lengths: &[usize]) -> Result<SegmentMix> {
    SegmentMix::try_new(segments, accepted_row_raw_lengths)
}

fn maximum_rows() -> [usize; 32] {
    let mut rows = [8_191; 32];
    rows[31] = 8_223;
    rows
}

fn canonical_metadata(generation: u8, is_initial: bool) -> usize {
    let (kind, source_attempt) = if is_initial {
        (
            SegmentKindV1::InitialAgentMessage,
            Some(SourceAttemptId::new([3; 16])),
        )
    } else {
        (SegmentKindV1::AgentMessage, None)
    };
    let record = RecordV1::AcceptedAgentContent(AcceptedAgentContentRecord {
        generation: SegmentGeneration::try_new(generation).unwrap(),
        kind,
        message: AgentMessageId::new([1; 16]),
        sender_session: SenderSessionId::new([2; 16]),
        source_attempt,
        relation: AgentSenderRelationV1::ParentToOwnedDescendant,
        authority: AgentAuthorityV1::ModelAuthoredUntrusted,
        text: AgentText::try_new(b"").unwrap(),
        timestamp: Timestamp::try_new(9_999_999_999_999).unwrap(),
    });
    EncodedRecord::try_new(&record).unwrap().len()
}

#[test]
fn exact_and_aligned_maximum_accounting_is_frozen() {
    assert_eq!(
        account(mix(33, &maximum_rows()).unwrap()).unwrap(),
        JournalAccounting {
            control_slots: 238,
            control_exact: 30_629,
            control_aligned: 33_728,
            supersession_exact: 9_636,
            supersession_aligned: 10_560,
            accepted_exact: 354_121,
            accepted_aligned: 393_216,
            compact_exact: 394_386,
            logical_aligned: 437_504,
            old_append_exact: 478_522,
            old_append_aligned: 530_944,
            physical_exact: 874_844,
            physical_aligned: 970_496,
            logical_margin: 86_784,
            physical_margin: 78_080,
        }
    );
}

#[test]
fn segment_formulas_cover_zero_one_and_maximum() {
    for segments in [0, 1, 33] {
        let actual = account(mix(segments, &[]).unwrap()).unwrap();
        let segments = u64::from(segments);
        assert_eq!(actual.control_slots, 7 * segments + 7);
        assert_eq!(
            actual.control_exact,
            (702 * segments + 429) + (200 * segments + 434)
        );
        assert_eq!(actual.control_aligned, 992 * segments + 992);
        assert_eq!(actual.supersession_exact, 292 * segments);
        assert_eq!(actual.supersession_aligned, 320 * segments);
    }
}

#[test]
fn completion_directory_covers_zero_one_and_maximum() {
    // Exact path composes account_completion / account_recovery (97_218 / 78_013 at S=33).
    // Aligned path uses canonical ROW_BYTES `.1` limits; unknown recovery is one run.
    for (segments, exact_high_water, aligned_high_water) in [
        (0, 263_142, 263_232),
        (1, 273_674, 275_840),
        (33, 612_906, 679_296),
    ] {
        assert_eq!(
            account_completion_directory(segments).unwrap(),
            CompletionDirectoryAccounting {
                exact_high_water,
                aligned_high_water,
                exact_margin: MAX_COMPLETION_DIRECTORY_BYTES - exact_high_water,
                aligned_margin: MAX_COMPLETION_DIRECTORY_BYTES - aligned_high_water,
            }
        );
    }
    let maximum = account_completion_directory(33).unwrap();
    assert_eq!(maximum.exact_margin, 173_526);
    assert_eq!(maximum.aligned_margin, 107_136);
}

#[test]
fn accepted_rows_and_directory_reject_invalid_bounds() {
    assert!(mix(34, &[]).is_err());
    assert!(mix(1, &[0, 0]).is_err());
    assert!(mix(33, &[0; 33]).is_err());
    assert!(mix(2, &[32_769, 0]).is_err());
    assert!(mix(9, &[32_768; 9]).is_err());
    assert!(account_completion_directory(34).is_err());
    assert!(account_completion_directory(u64::MAX).is_err());
    assert_eq!(
        checked_product(u64::MAX, 2),
        Err(CodecError::Invalid("accounting overflow"))
    );
}

#[test]
fn canonical_layout_and_attached_partition_fit_the_same_reservations() {
    assert_eq!(SourceAttempt::ENCODED_HEX_WIDTH, 32);
    assert_eq!(ACCEPTED_SOURCE_FIELD_OVERHEAD_BYTES, 7);
    let initial = canonical_metadata(1, true) as u64;
    let later = |generations: std::ops::RangeInclusive<u8>| {
        generations
            .map(|generation| canonical_metadata(generation, false) as u64)
            .sum::<u64>()
    };
    assert_eq!(accepted_metadata(1, 1).unwrap(), initial);
    assert_eq!(accepted_metadata(9, 9).unwrap(), initial + later(2..=9));
    assert_eq!(accepted_metadata(10, 10).unwrap(), initial + later(2..=10));

    let maximum_bound = accepted_metadata(33, 32).unwrap();
    let original_layout = later(2..=33);
    let continuation_with_attached_layout = initial + later(3..=33);
    assert!(original_layout < maximum_bound);
    assert_eq!(continuation_with_attached_layout, maximum_bound);

    let maximum = account(mix(33, &maximum_rows()).unwrap()).unwrap();
    let attached_partition = account(mix(33, &[8_192; 7]).unwrap()).unwrap();
    assert_eq!(attached_partition.control_slots, maximum.control_slots);
    assert_eq!(attached_partition.control_exact, maximum.control_exact);
    assert_eq!(attached_partition.control_aligned, maximum.control_aligned);
    assert_eq!(
        attached_partition.supersession_exact,
        maximum.supersession_exact
    );
    assert_eq!(
        attached_partition.supersession_aligned,
        maximum.supersession_aligned
    );
}

#[test]
fn thirty_two_row_residue_partition_proves_base64_maximum() {
    let row_lengths = maximum_rows();
    assert_eq!(row_lengths.iter().sum::<usize>(), 262_144);
    assert!(row_lengths.iter().all(|length| *length <= 32_768));
    assert_eq!(
        row_lengths
            .iter()
            .map(|length| (4 * length).div_ceil(3))
            .sum::<usize>(),
        349_546
    );
    let maximum = account(mix(33, &row_lengths).unwrap()).unwrap();
    assert_eq!(maximum.accepted_exact, 349_546 + 4_575);
    assert!(maximum.accepted_exact < maximum.accepted_aligned);
    assert!(maximum.compact_exact < maximum.logical_aligned);
    assert!(maximum.physical_exact < maximum.physical_aligned);
}
