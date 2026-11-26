//! Canonical completion-effect records.

use serde_json::Value;

use super::{
    codec::{
        CodecError, DomainBytes, MAX_SEGMENTS, Result, SegmentGeneration, Timestamp, encode_hex,
    },
    decoder::{
        RecordFamilyV1, decode_fixed_hex, preparse_bounded_row, small_unsigned, timestamp, unsigned,
    },
};

pub(super) const COMPLETION_ROW_BYTES: [(usize, usize); 3] = [(148, 160), (116, 128), (110, 128)];
const PROGRESS_ROWS_PER_GENERATION: u64 = CompletionEffectV1::COUNT as u64;
const PRODUCT_CLAIMS_PER_GENERATION: u64 = ProductClaimSlotV1::COUNT as u64;
const COMPLETE_ROWS_PER_GENERATION: u64 = 1;
const ONE_DIGIT_GENERATION_COUNT: u64 = 9;
/// Progress exact max is measured at two-digit `i`; effects `0..=9` are one digit.
const FIRST_TWO_DIGIT_EFFECT: u8 = 10;
const ONE_DIGIT_EFFECT_COUNT: u64 = FIRST_TWO_DIGIT_EFFECT as u64;
const TWO_DIGIT_EFFECT_COUNT: u64 = CompletionEffectV1::COUNT as u64 - ONE_DIGIT_EFFECT_COUNT;

macro_rules! domain {
    ($name:ident, $marker:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(super) enum $marker {}
        pub(super) type $name = DomainBytes<32, $marker>;
    };
}
domain!(EffectPayloadHash, EffectPayload);
domain!(ProductDescriptorPayloadHash, ProductDescriptorPayload);
domain!(CompletionAppliedSetHash, CompletionAppliedSet);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum CompletionEffectV1 {
    ConversationFence = 0,
    RewindPoint = 1,
    Signals = 2,
    Plan = 3,
    Goal = 4,
    Usage = 5,
    Prompt = 6,
    Resources = 7,
    Reparent = 8,
    AcceptedInputs = 9,
    TurnCompleted = 10,
    AttemptProjection = 11,
    QueueCut = 12,
}
impl CompletionEffectV1 {
    pub(super) const COUNT: u8 = 13;
    pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::ConversationFence),
            1 => Ok(Self::RewindPoint),
            2 => Ok(Self::Signals),
            3 => Ok(Self::Plan),
            4 => Ok(Self::Goal),
            5 => Ok(Self::Usage),
            6 => Ok(Self::Prompt),
            7 => Ok(Self::Resources),
            8 => Ok(Self::Reparent),
            9 => Ok(Self::AcceptedInputs),
            10 => Ok(Self::TurnCompleted),
            11 => Ok(Self::AttemptProjection),
            12 => Ok(Self::QueueCut),
            _ => Err(CodecError::Invalid("completion effect")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProductClaimSlotV1(u8);
impl ProductClaimSlotV1 {
    pub(super) const COUNT: u8 = 8;
    pub(super) fn try_new(value: u8) -> Result<Self> {
        (value < Self::COUNT)
            .then_some(Self(value))
            .ok_or(CodecError::Invalid("product claim slot"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum ProgressOutcomeV1 {
    Applied = 0,
    Superseded = 1,
    AlreadyApplied = 2,
}
impl ProgressOutcomeV1 {
    pub(super) const COUNT: u8 = 3;
    pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Applied),
            1 => Ok(Self::Superseded),
            2 => Ok(Self::AlreadyApplied),
            _ => Err(CodecError::Invalid("progress outcome")),
        }
    }
}

macro_rules! records {
    ($($name:ident { $($field:ident: $ty:ty),+ $(,)? }),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(super) struct $name { $(pub(super) $field: $ty),+ }
    )+};
}
records! {
    CompletionProgress { generation: SegmentGeneration, effect: CompletionEffectV1, payload_hash: EffectPayloadHash, outcome: ProgressOutcomeV1, observed_revision: u64, timestamp: Timestamp },
    CompletionProductClaim { generation: SegmentGeneration, slot: ProductClaimSlotV1, payload_hash: ProductDescriptorPayloadHash, timestamp: Timestamp },
    CompletionComplete { generation: SegmentGeneration, applied_set_hash: CompletionAppliedSetHash, timestamp: Timestamp },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CompletionRecordV1 {
    Progress(CompletionProgress),
    ProductClaim(CompletionProductClaim),
    Complete(CompletionComplete),
}
impl CompletionRecordV1 {
    fn event(&self) -> u8 {
        match self {
            Self::Progress(_) => 0,
            Self::ProductClaim(_) => 1,
            Self::Complete(_) => 2,
        }
    }
    pub(super) fn limits(&self) -> (usize, usize) {
        COMPLETION_ROW_BYTES[usize::from(self.event())]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EncodedCompletionRecord(Vec<u8>);
impl EncodedCompletionRecord {
    pub(super) fn try_new(record: &CompletionRecordV1) -> Result<Self> {
        let mut bytes = serde_json::to_vec(&wire(record)).map_err(|_| CodecError::Json)?;
        bytes.push(b'\n');
        let (_, max) = record.limits();
        if bytes.len() > max {
            return Err(CodecError::Limit {
                field: "encoded completion record",
                actual: bytes.len(),
                max,
            });
        }
        Ok(Self(bytes))
    }
    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

fn wire(record: &CompletionRecordV1) -> Value {
    match record {
        CompletionRecordV1::Progress(v) => {
            serde_json::json!({"v":1,"e":0,"g":v.generation.value(),"i":v.effect as u8,"h":encode_hex(v.payload_hash.as_bytes()),"o":v.outcome as u8,"r":v.observed_revision,"t":v.timestamp.value()})
        }
        CompletionRecordV1::ProductClaim(v) => {
            serde_json::json!({"v":1,"e":1,"g":v.generation.value(),"i":v.slot.0,"h":encode_hex(v.payload_hash.as_bytes()),"t":v.timestamp.value()})
        }
        CompletionRecordV1::Complete(v) => {
            serde_json::json!({"v":1,"e":2,"g":v.generation.value(),"h":encode_hex(v.applied_set_hash.as_bytes()),"t":v.timestamp.value()})
        }
    }
}

pub(super) fn decode_completion_record(bytes: &[u8]) -> Result<CompletionRecordV1> {
    let row = preparse_bounded_row(
        bytes,
        RecordFamilyV1::Completion,
        COMPLETION_ROW_BYTES[0].1,
        |event| {
            COMPLETION_ROW_BYTES
                .get(usize::from(event))
                .map(|row| row.1)
        },
        |_, _| Ok(()),
    )?;
    let fields = &row.fields;
    let generation =
        || SegmentGeneration::try_new(small_unsigned(fields, "g", "completion generation")?);
    let record = match row.event {
        0 => CompletionRecordV1::Progress(CompletionProgress {
            generation: generation()?,
            effect: CompletionEffectV1::try_from_ordinal(small_unsigned(
                fields,
                "i",
                "completion effect",
            )?)?,
            payload_hash: DomainBytes::new(decode_fixed_hex(fields, "h", "effect payload hash")?),
            outcome: ProgressOutcomeV1::try_from_ordinal(small_unsigned(
                fields,
                "o",
                "progress outcome",
            )?)?,
            observed_revision: unsigned(fields, "r", "observed revision")?,
            timestamp: timestamp(fields)?,
        }),
        1 => CompletionRecordV1::ProductClaim(CompletionProductClaim {
            generation: generation()?,
            slot: ProductClaimSlotV1::try_new(small_unsigned(fields, "i", "product claim slot")?)?,
            payload_hash: DomainBytes::new(decode_fixed_hex(
                fields,
                "h",
                "product descriptor payload hash",
            )?),
            timestamp: timestamp(fields)?,
        }),
        2 => CompletionRecordV1::Complete(CompletionComplete {
            generation: generation()?,
            applied_set_hash: DomainBytes::new(decode_fixed_hex(
                fields,
                "h",
                "completion applied-set hash",
            )?),
            timestamp: timestamp(fields)?,
        }),
        _ => return Err(CodecError::Invalid("completion event")),
    };
    if EncodedCompletionRecord::try_new(&record)?.as_bytes() != bytes {
        return Err(CodecError::Invalid("canonical completion record"));
    }
    Ok(record)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompletionAccounting {
    pub(super) rows: u64,
    pub(super) exact_bytes: u64,
}
pub(super) fn account_completion(segments: u64) -> Result<CompletionAccounting> {
    if segments > u64::from(MAX_SEGMENTS) {
        return Err(CodecError::Invalid("segment count"));
    }
    let multiply = |left: u64, right: u64| {
        left.checked_mul(right)
            .ok_or(CodecError::Invalid("accounting overflow"))
    };
    let add = |left: u64, right: u64| {
        left.checked_add(right)
            .ok_or(CodecError::Invalid("accounting overflow"))
    };
    let rows_per_generation =
        PROGRESS_ROWS_PER_GENERATION + PRODUCT_CLAIMS_PER_GENERATION + COMPLETE_ROWS_PER_GENERATION;
    // Exact Progress aggregate over the legal effect vocabulary (0..=9 one-digit `i`).
    let progress_two_digit_generation_bytes = ONE_DIGIT_EFFECT_COUNT
        * (COMPLETION_ROW_BYTES[0].0 as u64 - 1)
        + TWO_DIGIT_EFFECT_COUNT * COMPLETION_ROW_BYTES[0].0 as u64;
    let two_digit_bytes = progress_two_digit_generation_bytes
        + PRODUCT_CLAIMS_PER_GENERATION * COMPLETION_ROW_BYTES[1].0 as u64
        + COMPLETE_ROWS_PER_GENERATION * COMPLETION_ROW_BYTES[2].0 as u64;
    let one_digit_bytes = two_digit_bytes - rows_per_generation;
    let one_digit = segments.min(ONE_DIGIT_GENERATION_COUNT);
    let two_digit = segments - one_digit;
    Ok(CompletionAccounting {
        rows: multiply(segments, rows_per_generation)?,
        exact_bytes: add(
            multiply(one_digit, one_digit_bytes)?,
            multiply(two_digit, two_digit_bytes)?,
        )?,
    })
}
