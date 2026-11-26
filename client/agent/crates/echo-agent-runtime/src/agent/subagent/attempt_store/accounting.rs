//! Exact and aligned maximum-size proofs for attempt storage.

use super::{
    codec::{
        ACCEPTED_SOURCE_FIELD_OVERHEAD_BYTES, CodecError, MAX_ACCEPTED_CONTENT_BYTES,
        MAX_ACCEPTED_RAW_BYTES, MAX_ACCEPTED_ROWS, MAX_COMPLETION_DIRECTORY_BYTES,
        MAX_JOURNAL_LOGICAL_BYTES, MAX_JOURNAL_PHYSICAL_BYTES, MAX_MESSAGE_RAW_BYTES, MAX_SEGMENTS,
        ROW_LIMITS, SourceAttempt,
    },
    completion::{
        COMPLETION_ROW_BYTES, CompletionEffectV1, ProductClaimSlotV1, account_completion,
    },
    intent::{INTENT_PAIR_ALIGNED_BYTES, INTENT_PAIR_EXACT_BYTES},
    recovery::{RECOVERY_ROW_BYTES, RecoveryRunV1, account_recovery},
    rewind::A2_EXACT_ALIGNED_ROW_BYTES,
};

const A1_PER_SEGMENT: [usize; 6] = [3, 4, 5, 6, 7, 8];
const A1_FIXED: [usize; 4] = [0, 1, 9, 10];
const A2_PER_SEGMENT: [usize; 1] = [0];
const A2_FIXED: [usize; 3] = [2, 3, 4];
const ACCEPTED_CONTENT_ROW: usize = 2;
const A2_SUPERSESSION_ROW: usize = 1;
const A1_PER_EXACT: u64 = row_sum(&ROW_LIMITS, &A1_PER_SEGMENT, false);
const A1_FIXED_EXACT: u64 = row_sum(&ROW_LIMITS, &A1_FIXED, false);
const A2_PER_EXACT: u64 = row_sum(&A2_EXACT_ALIGNED_ROW_BYTES, &A2_PER_SEGMENT, false);
const A2_FIXED_EXACT: u64 = row_sum(&A2_EXACT_ALIGNED_ROW_BYTES, &A2_FIXED, false);
const CONTROL_PER_ALIGNED: u64 = row_sum(&ROW_LIMITS, &A1_PER_SEGMENT, true)
    + row_sum(&A2_EXACT_ALIGNED_ROW_BYTES, &A2_PER_SEGMENT, true);
const CONTROL_FIXED_ALIGNED: u64 =
    row_sum(&ROW_LIMITS, &A1_FIXED, true) + row_sum(&A2_EXACT_ALIGNED_ROW_BYTES, &A2_FIXED, true);
const ACCEPTED_ROW_EXACT: u64 = ROW_LIMITS[ACCEPTED_CONTENT_ROW].0 as u64;
const ACCEPTED_ROW_ALIGNED: u64 = ROW_LIMITS[ACCEPTED_CONTENT_ROW].1 as u64;
const MAX_RAW_ROW_BYTES: u64 = MAX_MESSAGE_RAW_BYTES as u64;
const INITIAL_METADATA: u64 = ACCEPTED_ROW_EXACT - (4 * MAX_RAW_ROW_BYTES).div_ceil(3);
const SOURCE_FIELD_ENCODED_BYTES: u64 =
    (SourceAttempt::ENCODED_HEX_WIDTH + ACCEPTED_SOURCE_FIELD_OVERHEAD_BYTES) as u64;
const LATER_ONE_DIGIT_METADATA: u64 = INITIAL_METADATA - SOURCE_FIELD_ENCODED_BYTES;
const FIRST_TWO_DIGIT_GENERATION: u8 = 10;
const TWO_DIGIT_GENERATION_BYTE_INCREMENT: u64 = 1;
// Aligned reserves consume canonical `.1` codec limits from the row-byte tuples —
// not `align16(exact)`. Complete and Claim `.1` exceed `align16(.0)`.
const COMPLETION_ALIGNED_PER_GENERATION: u64 = CompletionEffectV1::COUNT as u64
    * COMPLETION_ROW_BYTES[0].1 as u64
    + ProductClaimSlotV1::COUNT as u64 * COMPLETION_ROW_BYTES[1].1 as u64
    + COMPLETION_ROW_BYTES[2].1 as u64;
const RECOVERY_ALIGNED_PER_RUN: u64 = RECOVERY_ROW_BYTES[0].1 as u64
    + RECOVERY_ROW_BYTES[1].1 as u64
    + RECOVERY_ROW_BYTES[2].1 as u64;
const RECOVERY_ALIGNED_PER_KNOWN_SLOT: u64 = RecoveryRunV1::COUNT as u64 * RECOVERY_ALIGNED_PER_RUN;
// Unknown permits only terminal run 7; all three record kinds remain on that key.
const RECOVERY_ALIGNED_UNKNOWN_TERMINAL: u64 = RECOVERY_ALIGNED_PER_RUN;
// A crash can retain both sidecar images plus one torn row from the replacement.
const COMPLETION_CRASH_OVERLAP_COPIES: u64 = 2;
const RECOVERY_CRASH_OVERLAP_COPIES: u64 = 2;
const COMPLETION_PROGRESS_ROW_INDEX: usize = 0;
const RECOVERY_OUTCOME_ROW_INDEX: usize = 1;
const COMPLETION_CORE_FILE_COUNT: u64 = 3;
const COMPLETION_CORE_FILE_BYTES: u64 = 64 * 1_024;
const FIXED_COMPLETION_DIRECTORY_SLOT_BYTES: u64 = 64 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcceptedRows {
    count: u8,
    encoded_content_bytes: u64,
}
impl AcceptedRows {
    fn try_new(raw_lengths: &[usize]) -> Result<Self, CodecError> {
        if raw_lengths.len() > usize::from(MAX_ACCEPTED_ROWS) {
            return Err(CodecError::Invalid("accepted row count"));
        }
        let mut raw_bytes = 0;
        let mut encoded_content_bytes = 0;
        for raw_length in raw_lengths {
            if *raw_length > MAX_MESSAGE_RAW_BYTES {
                return Err(CodecError::Invalid("accepted row raw bytes"));
            }
            let raw_length = u64::try_from(*raw_length).map_err(|_| overflow())?;
            raw_bytes = checked_sum(&[raw_bytes, raw_length])?;
            encoded_content_bytes = checked_sum(&[
                encoded_content_bytes,
                checked_product(4, raw_length)?.div_ceil(3),
            ])?;
        }
        if raw_bytes > MAX_ACCEPTED_RAW_BYTES {
            return Err(CodecError::Invalid("accepted raw bytes"));
        }
        Ok(Self {
            count: u8::try_from(raw_lengths.len()).map_err(|_| overflow())?,
            encoded_content_bytes,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SegmentMix {
    segments: u8,
    accepted_rows: AcceptedRows,
}
impl SegmentMix {
    pub(super) fn try_new(
        segments: u8,
        accepted_row_raw_lengths: &[usize],
    ) -> Result<Self, CodecError> {
        if segments > MAX_SEGMENTS {
            return Err(CodecError::Invalid("segment count"));
        }
        let accepted_rows = AcceptedRows::try_new(accepted_row_raw_lengths)?;
        if accepted_rows.count > segments {
            return Err(CodecError::Invalid("accepted row count"));
        }
        Ok(Self {
            segments,
            accepted_rows,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct JournalAccounting {
    pub(super) control_slots: u64,
    pub(super) control_exact: u64,
    pub(super) control_aligned: u64,
    pub(super) supersession_exact: u64,
    pub(super) supersession_aligned: u64,
    pub(super) accepted_exact: u64,
    pub(super) accepted_aligned: u64,
    pub(super) compact_exact: u64,
    pub(super) logical_aligned: u64,
    pub(super) old_append_exact: u64,
    pub(super) old_append_aligned: u64,
    pub(super) physical_exact: u64,
    pub(super) physical_aligned: u64,
    pub(super) logical_margin: u64,
    pub(super) physical_margin: u64,
}

pub(super) fn account(mix: SegmentMix) -> Result<JournalAccounting, CodecError> {
    let segments = u64::from(mix.segments);
    let accepted_rows = u64::from(mix.accepted_rows.count);
    let a1_control_exact = checked_multiply_add(segments, A1_PER_EXACT, A1_FIXED_EXACT)?;
    let a2_control_exact = checked_multiply_add(segments, A2_PER_EXACT, A2_FIXED_EXACT)?;
    let control_exact = checked_sum(&[a1_control_exact, a2_control_exact])?;
    let control_aligned =
        checked_multiply_add(segments, CONTROL_PER_ALIGNED, CONTROL_FIXED_ALIGNED)?;
    let control_slots = checked_multiply_add(
        segments,
        (A1_PER_SEGMENT.len() + A2_PER_SEGMENT.len()) as u64,
        (A1_FIXED.len() + A2_FIXED.len()) as u64,
    )?;
    let (supersession_exact_row, supersession_aligned_row) =
        A2_EXACT_ALIGNED_ROW_BYTES[A2_SUPERSESSION_ROW];
    let supersession_exact = checked_product(segments, supersession_exact_row as u64)?;
    let supersession_aligned = checked_product(segments, supersession_aligned_row as u64)?;
    let accepted_metadata_exact = accepted_metadata(mix.segments, mix.accepted_rows.count)?;
    let accepted_exact = checked_sum(&[
        mix.accepted_rows.encoded_content_bytes,
        accepted_metadata_exact,
    ])?;
    let accepted_aligned =
        checked_product(accepted_rows, ACCEPTED_ROW_ALIGNED)?.min(MAX_ACCEPTED_CONTENT_BYTES);
    let compact_exact = checked_sum(&[accepted_exact, control_exact, supersession_exact])?;
    let logical_aligned = checked_sum(&[accepted_aligned, control_aligned, supersession_aligned])?;
    let (torn_exact, torn_aligned) = if mix.accepted_rows.count == 0 {
        (0, 0)
    } else {
        (ACCEPTED_ROW_EXACT, ACCEPTED_ROW_ALIGNED)
    };
    let old_append_exact = checked_sum(&[
        accepted_exact,
        checked_product(2, control_exact)?,
        checked_product(2, supersession_exact)?,
        torn_exact,
    ])?;
    let old_append_aligned = checked_sum(&[
        accepted_aligned,
        checked_product(2, control_aligned)?,
        checked_product(2, supersession_aligned)?,
        torn_aligned,
    ])?;
    let physical_exact = checked_sum(&[old_append_exact, compact_exact, INTENT_PAIR_EXACT_BYTES])?;
    let physical_aligned = checked_sum(&[
        old_append_aligned,
        logical_aligned,
        INTENT_PAIR_ALIGNED_BYTES,
    ])?;
    let logical_margin = MAX_JOURNAL_LOGICAL_BYTES
        .checked_sub(logical_aligned)
        .ok_or(CodecError::Invalid("logical journal cap"))?;
    let physical_margin = MAX_JOURNAL_PHYSICAL_BYTES
        .checked_sub(physical_aligned)
        .ok_or(CodecError::Invalid("physical journal cap"))?;
    Ok(JournalAccounting {
        control_slots,
        control_exact,
        control_aligned,
        supersession_exact,
        supersession_aligned,
        accepted_exact,
        accepted_aligned,
        compact_exact,
        logical_aligned,
        old_append_exact,
        old_append_aligned,
        physical_exact,
        physical_aligned,
        logical_margin,
        physical_margin,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompletionDirectoryAccounting {
    pub(super) exact_high_water: u64,
    pub(super) aligned_high_water: u64,
    pub(super) exact_margin: u64,
    pub(super) aligned_margin: u64,
}

pub(super) fn account_completion_directory(
    segments: u64,
) -> Result<CompletionDirectoryAccounting, CodecError> {
    if segments > u64::from(MAX_SEGMENTS) {
        return Err(CodecError::Invalid("segment count"));
    }
    let completion_exact = account_completion(segments)?.exact_bytes;
    let recovery_exact = account_recovery(segments)?.exact_bytes;
    let completion_aligned = checked_product(segments, COMPLETION_ALIGNED_PER_GENERATION)?;
    let recovery_aligned = checked_multiply_add(
        segments,
        RECOVERY_ALIGNED_PER_KNOWN_SLOT,
        RECOVERY_ALIGNED_UNKNOWN_TERMINAL,
    )?;
    let exact_high_water = checked_sum(&[
        checked_product(COMPLETION_CRASH_OVERLAP_COPIES, completion_exact)?,
        COMPLETION_ROW_BYTES[COMPLETION_PROGRESS_ROW_INDEX].0 as u64,
        checked_product(RECOVERY_CRASH_OVERLAP_COPIES, recovery_exact)?,
        RECOVERY_ROW_BYTES[RECOVERY_OUTCOME_ROW_INDEX].0 as u64,
        checked_product(COMPLETION_CORE_FILE_COUNT, COMPLETION_CORE_FILE_BYTES)?,
        FIXED_COMPLETION_DIRECTORY_SLOT_BYTES,
    ])?;
    let aligned_high_water = checked_sum(&[
        checked_product(COMPLETION_CRASH_OVERLAP_COPIES, completion_aligned)?,
        COMPLETION_ROW_BYTES[COMPLETION_PROGRESS_ROW_INDEX].1 as u64,
        checked_product(RECOVERY_CRASH_OVERLAP_COPIES, recovery_aligned)?,
        RECOVERY_ROW_BYTES[RECOVERY_OUTCOME_ROW_INDEX].1 as u64,
        checked_product(COMPLETION_CORE_FILE_COUNT, COMPLETION_CORE_FILE_BYTES)?,
        FIXED_COMPLETION_DIRECTORY_SLOT_BYTES,
    ])?;
    Ok(CompletionDirectoryAccounting {
        exact_high_water,
        aligned_high_water,
        exact_margin: MAX_COMPLETION_DIRECTORY_BYTES
            .checked_sub(exact_high_water)
            .ok_or(CodecError::Invalid("completion directory cap"))?,
        aligned_margin: MAX_COMPLETION_DIRECTORY_BYTES
            .checked_sub(aligned_high_water)
            .ok_or(CodecError::Invalid("completion directory cap"))?,
    })
}

pub(super) fn accepted_metadata(segments: u8, accepted_rows: u8) -> Result<u64, CodecError> {
    if accepted_rows == 0 {
        return Ok(0);
    }
    let later_rows = u64::from(accepted_rows - 1);
    let one_digit_generations = FIRST_TWO_DIGIT_GENERATION - 1;
    let two_digit_rows = later_rows.min(u64::from(segments.saturating_sub(one_digit_generations)));
    checked_sum(&[
        INITIAL_METADATA,
        checked_product(
            two_digit_rows,
            LATER_ONE_DIGIT_METADATA + TWO_DIGIT_GENERATION_BYTE_INCREMENT,
        )?,
        checked_product(later_rows - two_digit_rows, LATER_ONE_DIGIT_METADATA)?,
    ])
}

const fn row_sum(table: &[(usize, usize)], indexes: &[usize], is_aligned: bool) -> u64 {
    let mut total = 0;
    let mut offset = 0;
    while offset < indexes.len() {
        let row = table[indexes[offset]];
        total += (if is_aligned { row.1 } else { row.0 }) as u64;
        offset += 1;
    }
    total
}

pub(super) fn checked_product(left: u64, right: u64) -> Result<u64, CodecError> {
    left.checked_mul(right).ok_or_else(overflow)
}
fn checked_multiply_add(value: u64, per: u64, fixed: u64) -> Result<u64, CodecError> {
    checked_sum(&[checked_product(value, per)?, fixed])
}
fn checked_sum(values: &[u64]) -> Result<u64, CodecError> {
    values.iter().try_fold(0_u64, |total, value| {
        total.checked_add(*value).ok_or_else(overflow)
    })
}
fn overflow() -> CodecError {
    CodecError::Invalid("accounting overflow")
}
