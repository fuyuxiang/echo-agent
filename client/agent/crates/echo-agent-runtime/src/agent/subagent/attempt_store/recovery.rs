//! Canonical bounded recovery-outcome records.

use serde_json::Value;

use super::{
    codec::{
        CodecError, DomainBytes, MAX_SEGMENTS, Result, SegmentGeneration, Timestamp,
        TurnResolutionV1, encode_hex,
    },
    decoder::{
        RecordFamilyV1, decode_fixed_hex, preparse_bounded_row, small_unsigned, timestamp, unsigned,
    },
};

pub(super) const RECOVERY_ROW_BYTES: [(usize, usize); 3] = [(63, 64), (152, 160), (134, 160)];
const RECOVERY_RUNS_PER_SLOT: u64 = RecoveryRunV1::COUNT as u64;
const RECORDS_PER_RECOVERY_RUN: u64 = RECOVERY_ROW_BYTES.len() as u64;
const ONE_DIGIT_GENERATION_COUNT: u64 = 9;
const MAX_GENERATION_DECIMAL_DIGITS: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryOutcomePayload {}
pub(super) type RecoveryOutcomePayloadHash = DomainBytes<32, RecoveryOutcomePayload>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryGenerationV1 {
    Known(SegmentGeneration),
    Unknown,
}
impl RecoveryGenerationV1 {
    pub(super) fn try_new(value: u64) -> Result<Self> {
        if value == u64::MAX {
            Ok(Self::Unknown)
        } else {
            value
                .try_into()
                .map_err(|_| CodecError::Invalid("recovery generation"))
                .and_then(SegmentGeneration::try_new)
                .map(Self::Known)
        }
    }
    fn value(self) -> u64 {
        match self {
            Self::Known(generation) => u64::from(generation.value()),
            Self::Unknown => u64::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecoveryRunV1(u8);
impl RecoveryRunV1 {
    pub(super) const COUNT: u8 = 8;
    pub(super) fn try_new(value: u8) -> Result<Self> {
        (value < Self::COUNT)
            .then_some(Self(value))
            .ok_or(CodecError::Invalid("recovery run"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryRunClassV1 {
    OrdinaryMutable,
    TerminalQuarantine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecoveryRunKeyV1 {
    generation: RecoveryGenerationV1,
    run: RecoveryRunV1,
}
impl RecoveryRunKeyV1 {
    pub(super) fn try_new(generation: RecoveryGenerationV1, run: RecoveryRunV1) -> Result<Self> {
        match (generation, run.0) {
            (RecoveryGenerationV1::Known(_), 0..=6) => Ok(Self { generation, run }),
            (_, 7) => Ok(Self { generation, run }),
            _ => Err(CodecError::Invalid("recovery run key")),
        }
    }
    pub(super) fn classify(self) -> RecoveryRunClassV1 {
        match self.run.0 {
            0..=6 => RecoveryRunClassV1::OrdinaryMutable,
            _ => RecoveryRunClassV1::TerminalQuarantine,
        }
    }
}

macro_rules! closed_enum {
    ($name:ident, $field:literal, {$($variant:ident = $ordinal:literal),+ $(,)?}) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub(super) enum $name { $($variant = $ordinal),+ }
        impl $name {
            pub(super) const COUNT: u8 = [$(Self::$variant),+].len() as u8;
            pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
                match value {
                    $($ordinal => Ok(Self::$variant),)+
                    _ => Err(CodecError::Invalid($field)),
                }
            }
        }
    };
}
closed_enum!(RecoveryOutcomeV1, "recovery outcome", {
    Recovered = 0,
    CoreFailed = 1,
    Quarantined = 2,
});
closed_enum!(RecoveryRetryBucketV1, "recovery retry bucket", {
    Zero = 0,
    One = 1,
    Two = 2,
    Three = 3,
    Four = 4,
    Five = 5,
    Six = 6,
    EightPlus = 7,
});

macro_rules! records {
    ($($name:ident { $($field:ident: $ty:ty),+ $(,)? }),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(super) struct $name { $(pub(super) $field: $ty),+ }
    )+};
}
records! {
    RecoveryRunReserved { key: RecoveryRunKeyV1, timestamp: Timestamp },
    RecoveryOutcome { key: RecoveryRunKeyV1, payload_hash: RecoveryOutcomePayloadHash, resolution: TurnResolutionV1, outcome: RecoveryOutcomeV1, retry_bucket: RecoveryRetryBucketV1, timestamp: Timestamp },
    RecoveryClaim { key: RecoveryRunKeyV1, payload_hash: RecoveryOutcomePayloadHash, timestamp: Timestamp },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecoveryRecordV1 {
    RunReserved(RecoveryRunReserved),
    Outcome(RecoveryOutcome),
    Claim(RecoveryClaim),
}
impl RecoveryRecordV1 {
    fn event(&self) -> u8 {
        match self {
            Self::RunReserved(_) => 0,
            Self::Outcome(_) => 1,
            Self::Claim(_) => 2,
        }
    }
    pub(super) fn limits(&self) -> (usize, usize) {
        RECOVERY_ROW_BYTES[usize::from(self.event())]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EncodedRecoveryRecord(Vec<u8>);
impl EncodedRecoveryRecord {
    pub(super) fn try_new(record: &RecoveryRecordV1) -> Result<Self> {
        validate(record)?;
        let mut bytes = serde_json::to_vec(&wire(record)).map_err(|_| CodecError::Json)?;
        bytes.push(b'\n');
        let (_, max) = record.limits();
        if bytes.len() > max {
            return Err(CodecError::Limit {
                field: "encoded recovery record",
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

fn validate(record: &RecoveryRecordV1) -> Result<()> {
    let RecoveryRecordV1::Outcome(value) = record else {
        return Ok(());
    };
    match value.key.classify() {
        RecoveryRunClassV1::OrdinaryMutable
            if matches!(value.retry_bucket, RecoveryRetryBucketV1::EightPlus) =>
        {
            Err(CodecError::Invalid("ordinary recovery outcome"))
        }
        RecoveryRunClassV1::TerminalQuarantine
            if !matches!(value.outcome, RecoveryOutcomeV1::Quarantined)
                || !matches!(value.retry_bucket, RecoveryRetryBucketV1::EightPlus) =>
        {
            Err(CodecError::Invalid("terminal recovery outcome"))
        }
        _ => Ok(()),
    }
}

fn wire(record: &RecoveryRecordV1) -> Value {
    let hash = |value: &RecoveryOutcomePayloadHash| encode_hex(value.as_bytes());
    match record {
        RecoveryRecordV1::RunReserved(value) => {
            serde_json::json!({"v":1,"e":0,"g":value.key.generation.value(),"r":value.key.run.0,"t":value.timestamp.value()})
        }
        RecoveryRecordV1::Outcome(value) => {
            serde_json::json!({"v":1,"e":1,"g":value.key.generation.value(),"r":value.key.run.0,"h":hash(&value.payload_hash),"x":value.resolution as u8,"o":value.outcome as u8,"b":value.retry_bucket as u8,"t":value.timestamp.value()})
        }
        RecoveryRecordV1::Claim(value) => {
            serde_json::json!({"v":1,"e":2,"g":value.key.generation.value(),"r":value.key.run.0,"h":hash(&value.payload_hash),"t":value.timestamp.value()})
        }
    }
}

pub(super) fn decode_recovery_record(bytes: &[u8]) -> Result<RecoveryRecordV1> {
    let row = preparse_bounded_row(
        bytes,
        RecordFamilyV1::Recovery,
        RECOVERY_ROW_BYTES[1].1,
        |event| RECOVERY_ROW_BYTES.get(usize::from(event)).map(|row| row.1),
        |_, _| Ok(()),
    )?;
    let fields = &row.fields;
    let key = || {
        RecoveryRunKeyV1::try_new(
            RecoveryGenerationV1::try_new(unsigned(fields, "g", "recovery generation")?)?,
            RecoveryRunV1::try_new(small_unsigned(fields, "r", "recovery run")?)?,
        )
    };
    let payload_hash =
        || decode_fixed_hex(fields, "h", "recovery outcome payload hash").map(DomainBytes::new);
    let record = match row.event {
        0 => RecoveryRecordV1::RunReserved(RecoveryRunReserved {
            key: key()?,
            timestamp: timestamp(fields)?,
        }),
        1 => RecoveryRecordV1::Outcome(RecoveryOutcome {
            key: key()?,
            payload_hash: payload_hash()?,
            resolution: TurnResolutionV1::try_from_ordinal(small_unsigned(
                fields,
                "x",
                "turn resolution",
            )?)?,
            outcome: RecoveryOutcomeV1::try_from_ordinal(small_unsigned(
                fields,
                "o",
                "recovery outcome",
            )?)?,
            retry_bucket: RecoveryRetryBucketV1::try_from_ordinal(small_unsigned(
                fields,
                "b",
                "recovery retry bucket",
            )?)?,
            timestamp: timestamp(fields)?,
        }),
        2 => RecoveryRecordV1::Claim(RecoveryClaim {
            key: key()?,
            payload_hash: payload_hash()?,
            timestamp: timestamp(fields)?,
        }),
        _ => return Err(CodecError::Invalid("recovery event")),
    };
    if EncodedRecoveryRecord::try_new(&record)?.as_bytes() != bytes {
        return Err(CodecError::Invalid("canonical recovery record"));
    }
    Ok(record)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecoveryAccounting {
    pub(super) rows: u64,
    pub(super) exact_bytes: u64,
}
pub(super) fn account_recovery(segments: u64) -> Result<RecoveryAccounting> {
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
    let max_bytes_per_run = RECOVERY_ROW_BYTES
        .iter()
        .try_fold(0_u64, |sum, row| add(sum, row.0 as u64))?;
    let bytes_per_known_slot = |digits: u64| {
        let omitted_digits = MAX_GENERATION_DECIMAL_DIGITS - digits;
        let savings = multiply(RECORDS_PER_RECOVERY_RUN, omitted_digits)?;
        let bytes_per_run = max_bytes_per_run
            .checked_sub(savings)
            .ok_or(CodecError::Invalid("accounting overflow"))?;
        multiply(RECOVERY_RUNS_PER_SLOT, bytes_per_run)
    };
    // Known generations permit runs 0..=7. Unknown permits only terminal run 7,
    // but all three record kinds remain constructible on that sole legal key.
    let rows_per_known_slot = multiply(RECOVERY_RUNS_PER_SLOT, RECORDS_PER_RECOVERY_RUN)?;
    let unknown_terminal_rows = RECORDS_PER_RECOVERY_RUN;
    let unknown_terminal_bytes = max_bytes_per_run;
    let one_digit = segments.min(ONE_DIGIT_GENERATION_COUNT);
    let two_digit = segments - one_digit;
    let known_rows = multiply(segments, rows_per_known_slot)?;
    let known_bytes = add(
        multiply(one_digit, bytes_per_known_slot(1)?)?,
        multiply(two_digit, bytes_per_known_slot(2)?)?,
    )?;
    Ok(RecoveryAccounting {
        rows: add(known_rows, unknown_terminal_rows)?,
        exact_bytes: add(known_bytes, unknown_terminal_bytes)?,
    })
}
