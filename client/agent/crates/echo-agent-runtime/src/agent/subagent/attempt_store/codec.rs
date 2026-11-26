//! Canonical core attempt record types and encoder.

use std::marker::PhantomData;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

pub(super) const MAX_MESSAGE_RAW_BYTES: usize = 32 * 1024;
pub(super) const MAX_SEGMENTS: u8 = 33;
pub(super) const MAX_ACCEPTED_ROWS: u8 = 32;
pub(super) const MAX_ACCEPTED_RAW_BYTES: u64 = 256 * 1024;
pub(super) const MAX_ACCEPTED_CONTENT_BYTES: u64 = 384 * 1024;
pub(super) const MAX_JOURNAL_LOGICAL_BYTES: u64 = 512 * 1024;
pub(super) const MAX_JOURNAL_PHYSICAL_BYTES: u64 = 1024 * 1024;
pub(super) const MAX_COMPLETION_DIRECTORY_BYTES: u64 = 768 * 1024;
const MAX_TIMESTAMP: u64 = 9_999_999_999_999;

pub(super) type Result<T> = std::result::Result<T, CodecError>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum CodecError {
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("{field} is {actual} bytes; maximum is {max}")]
    Limit {
        field: &'static str,
        actual: usize,
        max: usize,
    },
    #[error("invalid JSON")]
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum SegmentKindV1 {
    InitialOriginalTask = 0,
    InitialAgentMessage = 1,
    AgentMessage = 2,
    AttachedHuman = 3,
}
impl SegmentKindV1 {
    pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::InitialOriginalTask),
            1 => Ok(Self::InitialAgentMessage),
            2 => Ok(Self::AgentMessage),
            3 => Ok(Self::AttachedHuman),
            _ => Err(CodecError::Invalid("segment kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum AgentSenderRelationV1 {
    ParentToOwnedDescendant = 0,
}
impl AgentSenderRelationV1 {
    pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::ParentToOwnedDescendant),
            _ => Err(CodecError::Invalid("sender relation")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum AgentAuthorityV1 {
    ModelAuthoredUntrusted = 0,
}
impl AgentAuthorityV1 {
    pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::ModelAuthoredUntrusted),
            _ => Err(CodecError::Invalid("agent authority")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum TurnResolutionV1 {
    Delivered = 0,
    Failed = 1,
    Cancelled = 2,
}
impl TurnResolutionV1 {
    pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Delivered),
            1 => Ok(Self::Failed),
            2 => Ok(Self::Cancelled),
            _ => Err(CodecError::Invalid("turn resolution")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum AdmissionCloseReasonV1 {
    Drained = 0,
    RuntimeFailure = 1,
    Cancellation = 2,
    Corruption = 3,
}
impl AdmissionCloseReasonV1 {
    pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Drained),
            1 => Ok(Self::RuntimeFailure),
            2 => Ok(Self::Cancellation),
            3 => Ok(Self::Corruption),
            _ => Err(CodecError::Invalid("admission close reason")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputDispositionV1 {
    Queued,
    Delivered,
    Failed,
    Cancelled,
}
impl InputDispositionV1 {
    fn ordinals(self) -> (u8, u8) {
        match self {
            Self::Queued => (0, 0),
            Self::Delivered => (1, 0),
            Self::Failed => (2, 1),
            Self::Cancelled => (3, 2),
        }
    }

    pub(super) fn try_from_ordinals(outcome: u8, reason: u8) -> Result<Self> {
        match (outcome, reason) {
            (0, 0) => Ok(Self::Queued),
            (1, 0) => Ok(Self::Delivered),
            (2, 1) => Ok(Self::Failed),
            (3, 2) => Ok(Self::Cancelled),
            _ => Err(CodecError::Invalid("input disposition product")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttemptOutcomeV1 {
    Completed,
    Failed,
    Cancelled,
    Quarantined,
}
impl AttemptOutcomeV1 {
    fn ordinals(self) -> (u8, u8) {
        match self {
            Self::Completed => (0, 0),
            Self::Failed => (1, 1),
            Self::Cancelled => (2, 2),
            Self::Quarantined => (3, 3),
        }
    }

    pub(super) fn try_from_ordinals(outcome: u8, reason: u8) -> Result<Self> {
        match (outcome, reason) {
            (0, 0) => Ok(Self::Completed),
            (1, 1) => Ok(Self::Failed),
            (2, 2) => Ok(Self::Cancelled),
            (3, 3) => Ok(Self::Quarantined),
            _ => Err(CodecError::Invalid("attempt outcome product")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DomainBytes<const N: usize, K>([u8; N], PhantomData<fn() -> K>);
impl<const N: usize, K> DomainBytes<N, K> {
    pub(super) fn new(bytes: [u8; N]) -> Self {
        Self(bytes, PhantomData)
    }
    pub(super) fn as_bytes(&self) -> &[u8; N] {
        &self.0
    }
}

macro_rules! domains {
    ($($name:ident: $marker:ident[$width:expr]),+ $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub(super) enum $marker {}
        pub(super) type $name = DomainBytes<$width, $marker>;
        impl $marker {
            pub(super) const ENCODED_HEX_WIDTH: usize = $width * 2;
        }
    )+};
}
domains! {
    AttemptId: Attempt[16], OwnershipRootId: OwnershipRoot[16], AgentMessageId: AgentMessage[16],
    SenderSessionId: SenderSession[16], SourceAttemptId: SourceAttempt[16], PromptId: Prompt[16],
    ConstructorDigest: Constructor[32], PayloadHash: Payload[32], ProjectionSetHash: ProjectionSet[32],
    CompletionCoreHash: CompletionCore[32], RewindRefHash: RewindRef[32],
}

pub(super) const ACCEPTED_SOURCE_FIELD_OVERHEAD_BYTES: usize = 7;

macro_rules! scalar {
    ($name:ident($ty:ty)) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(super) struct $name($ty);
        impl $name {
            pub(super) fn new(value: $ty) -> Self {
                Self(value)
            }
        }
    };
}
scalar!(BarrierGeneration(u64));
scalar!(QueueGeneration(u64));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AttemptGeneration(u64);
impl AttemptGeneration {
    pub(super) fn try_new(value: u64) -> Result<Self> {
        if value == 0 {
            Err(CodecError::Invalid("attempt generation"))
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SegmentGeneration(u8);
impl SegmentGeneration {
    pub(super) fn try_new(value: u8) -> Result<Self> {
        if (1..=MAX_SEGMENTS).contains(&value) {
            Ok(Self(value))
        } else {
            Err(CodecError::Invalid("segment generation"))
        }
    }
    pub(super) fn index(self) -> u8 {
        self.0 - 1
    }
    pub(super) fn value(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SegmentHighWater(u8);
impl SegmentHighWater {
    pub(super) fn try_new(value: u8) -> Result<Self> {
        if value <= MAX_SEGMENTS {
            Ok(Self(value))
        } else {
            Err(CodecError::Invalid("segment high water"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Timestamp(u64);
impl Timestamp {
    pub(super) fn try_new(value: u64) -> Result<Self> {
        if value > MAX_TIMESTAMP {
            Err(limit("timestamp", value.to_string().len(), 13))
        } else {
            Ok(Self(value))
        }
    }
    pub(super) fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentText(Vec<u8>);
impl AgentText {
    pub(super) fn try_new(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_MESSAGE_RAW_BYTES {
            return Err(limit("agent text", bytes.len(), MAX_MESSAGE_RAW_BYTES));
        }
        std::str::from_utf8(bytes).map_err(|_| CodecError::Invalid("agent text UTF-8"))?;
        Ok(Self(bytes.to_vec()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CapacityProfile {
    pub(super) segments: u8,
    pub(super) messages: u8,
    pub(super) raw_bytes: u64,
    pub(super) content_bytes: u64,
    pub(super) logical_bytes: u64,
    pub(super) physical_bytes: u64,
    pub(super) directory_bytes: u64,
}
impl CapacityProfile {
    fn validate(&self) -> Result<()> {
        for (field, actual, max) in [
            (
                "segments",
                u64::from(self.segments),
                u64::from(MAX_SEGMENTS),
            ),
            (
                "messages",
                u64::from(self.messages),
                u64::from(MAX_ACCEPTED_ROWS),
            ),
            ("raw bytes", self.raw_bytes, MAX_ACCEPTED_RAW_BYTES),
            (
                "content bytes",
                self.content_bytes,
                MAX_ACCEPTED_CONTENT_BYTES,
            ),
            (
                "logical bytes",
                self.logical_bytes,
                MAX_JOURNAL_LOGICAL_BYTES,
            ),
            (
                "physical bytes",
                self.physical_bytes,
                MAX_JOURNAL_PHYSICAL_BYTES,
            ),
            (
                "directory bytes",
                self.directory_bytes,
                MAX_COMPLETION_DIRECTORY_BYTES,
            ),
        ] {
            if actual > max {
                return Err(CodecError::Invalid(field));
            }
        }
        Ok(())
    }
}

macro_rules! records {
    ($($name:ident { $($field:ident: $ty:ty),+ $(,)? }),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Eq)] pub(super) struct $name { $(pub(super) $field: $ty),+ }
    )+};
}
records! {
    AttemptHeaderRecord { attempt: AttemptId, owner_root: OwnershipRootId, generation: AttemptGeneration, initial: SegmentKindV1, constructor_digest: ConstructorDigest, timestamp: Timestamp },
    CapacityReservedRecord { profile: CapacityProfile, timestamp: Timestamp },
    AcceptedAgentContentRecord { generation: SegmentGeneration, kind: SegmentKindV1, message: AgentMessageId, sender_session: SenderSessionId, source_attempt: Option<SourceAttemptId>, relation: AgentSenderRelationV1, authority: AgentAuthorityV1, text: AgentText, timestamp: Timestamp },
    SegmentReservedRecord { generation: SegmentGeneration, barrier: BarrierGeneration, kind: SegmentKindV1, prompt: PromptId, payload_hash: PayloadHash, timestamp: Timestamp },
    TurnStartedRecord { generation: SegmentGeneration, timestamp: Timestamp },
    TurnCommitIntentRecord { generation: SegmentGeneration, projection_hash: ProjectionSetHash, core_hash: CompletionCoreHash, timestamp: Timestamp },
    TurnResolutionRecord { generation: SegmentGeneration, resolution: TurnResolutionV1, core_hash: CompletionCoreHash, rewind_ref_hash: Option<RewindRefHash>, timestamp: Timestamp },
    InputDispositionRecord { generation: SegmentGeneration, disposition: InputDispositionV1, timestamp: Timestamp },
    QueueCutRecord { generation: SegmentGeneration, queue_generation: QueueGeneration, timestamp: Timestamp },
    AdmissionClosedRecord { barrier: BarrierGeneration, reason: AdmissionCloseReasonV1, timestamp: Timestamp },
    AttemptOutcomeRecord { high_water: SegmentHighWater, outcome: AttemptOutcomeV1, timestamp: Timestamp },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecordV1 {
    AttemptHeader(AttemptHeaderRecord),
    CapacityReserved(CapacityReservedRecord),
    AcceptedAgentContent(AcceptedAgentContentRecord),
    SegmentReserved(SegmentReservedRecord),
    TurnStarted(TurnStartedRecord),
    TurnCommitIntent(TurnCommitIntentRecord),
    TurnResolution(TurnResolutionRecord),
    InputDisposition(InputDispositionRecord),
    QueueCut(QueueCutRecord),
    AdmissionClosed(AdmissionClosedRecord),
    AttemptOutcome(AttemptOutcomeRecord),
}
pub(super) const ROW_LIMITS: [(usize, usize); 11] = [
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
    (52, 64),
];
pub(super) const MAX_ENCODED_RECORD_BYTES: usize = ROW_LIMITS[2].1;
impl RecordV1 {
    pub(super) fn limits(&self) -> (usize, usize) {
        ROW_LIMITS[usize::from(self.event())]
    }
    fn event(&self) -> u8 {
        match self {
            Self::AttemptHeader(_) => 0,
            Self::CapacityReserved(_) => 1,
            Self::AcceptedAgentContent(_) => 2,
            Self::SegmentReserved(_) => 3,
            Self::TurnStarted(_) => 4,
            Self::TurnCommitIntent(_) => 5,
            Self::TurnResolution(_) => 6,
            Self::InputDisposition(_) => 7,
            Self::QueueCut(_) => 8,
            Self::AdmissionClosed(_) => 9,
            Self::AttemptOutcome(_) => 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EncodedRecord(Vec<u8>);
impl EncodedRecord {
    pub(super) fn try_new(record: &RecordV1) -> Result<Self> {
        validate(record)?;
        let mut bytes = serde_json::to_vec(&wire(record)).map_err(|_| CodecError::Json)?;
        bytes.push(b'\n');
        let (_, cap) = record.limits();
        if bytes.len() > cap {
            return Err(limit("encoded record", bytes.len(), cap));
        }
        Ok(Self(bytes))
    }
    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }
}

fn validate(record: &RecordV1) -> Result<()> {
    match record {
        RecordV1::AttemptHeader(v)
            if matches!(
                v.initial,
                SegmentKindV1::InitialOriginalTask | SegmentKindV1::InitialAgentMessage
            ) => {}
        RecordV1::AttemptHeader(_) => return Err(CodecError::Invalid("initial segment kind")),
        RecordV1::CapacityReserved(v) => v.profile.validate()?,
        RecordV1::AcceptedAgentContent(v)
            if matches!(
                (v.generation.0, v.kind, &v.source_attempt),
                (1, SegmentKindV1::InitialAgentMessage, Some(_))
                    | (2..=33, SegmentKindV1::AgentMessage, None)
            ) => {}
        RecordV1::AcceptedAgentContent(_) => {
            return Err(CodecError::Invalid("agent message lineage"));
        }
        RecordV1::SegmentReserved(v)
            if matches!(
                (v.generation.0, v.kind),
                (
                    1,
                    SegmentKindV1::InitialOriginalTask | SegmentKindV1::InitialAgentMessage
                ) | (
                    2..=33,
                    SegmentKindV1::AgentMessage | SegmentKindV1::AttachedHuman
                )
            ) => {}
        RecordV1::SegmentReserved(_) => return Err(CodecError::Invalid("segment lineage")),
        RecordV1::TurnResolution(v)
            if matches!(v.resolution, TurnResolutionV1::Delivered)
                || v.rewind_ref_hash.is_none() => {}
        RecordV1::TurnResolution(_) => return Err(CodecError::Invalid("turn resolution ref")),
        RecordV1::TurnStarted(_)
        | RecordV1::TurnCommitIntent(_)
        | RecordV1::InputDisposition(_)
        | RecordV1::QueueCut(_)
        | RecordV1::AdmissionClosed(_)
        | RecordV1::AttemptOutcome(_) => {}
    }
    Ok(())
}

fn wire(record: &RecordV1) -> Value {
    let x = |value: &[u8]| encode_hex(value);
    match record {
        RecordV1::AttemptHeader(v) => {
            json!({"v":1,"e":0,"a":x(v.attempt.as_bytes()),"r":x(v.owner_root.as_bytes()),"g":v.generation.0,"k":v.initial as u8,"h":x(v.constructor_digest.as_bytes()),"t":v.timestamp.0})
        }
        RecordV1::CapacityReserved(v) => {
            let p = &v.profile;
            json!({"v":1,"e":1,"s":p.segments,"m":p.messages,"b":p.raw_bytes,"c":p.content_bytes,"l":p.logical_bytes,"p":p.physical_bytes,"d":p.directory_bytes,"t":v.timestamp.0})
        }
        RecordV1::AcceptedAgentContent(v) => match &v.source_attempt {
            Some(source) => {
                json!({"v":1,"e":2,"g":v.generation.0,"k":v.kind as u8,"m":x(v.message.as_bytes()),"s":x(v.sender_session.as_bytes()),"x":x(source.as_bytes()),"r":v.relation as u8,"a":v.authority as u8,"c":URL_SAFE_NO_PAD.encode(&v.text.0),"t":v.timestamp.0})
            }
            None => {
                json!({"v":1,"e":2,"g":v.generation.0,"k":v.kind as u8,"m":x(v.message.as_bytes()),"s":x(v.sender_session.as_bytes()),"r":v.relation as u8,"a":v.authority as u8,"c":URL_SAFE_NO_PAD.encode(&v.text.0),"t":v.timestamp.0})
            }
        },
        RecordV1::SegmentReserved(v) => {
            json!({"v":1,"e":3,"g":v.generation.0,"b":v.barrier.0,"k":v.kind as u8,"p":x(v.prompt.as_bytes()),"h":x(v.payload_hash.as_bytes()),"t":v.timestamp.0})
        }
        RecordV1::TurnStarted(v) => json!({"v":1,"e":4,"g":v.generation.0,"t":v.timestamp.0}),
        RecordV1::TurnCommitIntent(v) => {
            json!({"v":1,"e":5,"g":v.generation.0,"p":x(v.projection_hash.as_bytes()),"c":x(v.core_hash.as_bytes()),"t":v.timestamp.0})
        }
        RecordV1::TurnResolution(v) => match &v.rewind_ref_hash {
            Some(reference) => {
                json!({"v":1,"e":6,"g":v.generation.0,"o":v.resolution as u8,"c":x(v.core_hash.as_bytes()),"r":x(reference.as_bytes()),"t":v.timestamp.0})
            }
            None => {
                json!({"v":1,"e":6,"g":v.generation.0,"o":v.resolution as u8,"c":x(v.core_hash.as_bytes()),"t":v.timestamp.0})
            }
        },
        RecordV1::InputDisposition(v) => {
            let (o, r) = v.disposition.ordinals();
            json!({"v":1,"e":7,"g":v.generation.0,"o":o,"r":r,"t":v.timestamp.0})
        }
        RecordV1::QueueCut(v) => {
            json!({"v":1,"e":8,"g":v.generation.0,"q":v.queue_generation.0,"t":v.timestamp.0})
        }
        RecordV1::AdmissionClosed(v) => {
            json!({"v":1,"e":9,"b":v.barrier.0,"o":v.reason as u8,"t":v.timestamp.0})
        }
        RecordV1::AttemptOutcome(v) => {
            let (o, r) = v.outcome.ordinals();
            json!({"v":1,"e":10,"w":v.high_water.0,"o":o,"r":r,"t":v.timestamp.0})
        }
    }
}

fn limit(field: &'static str, actual: usize, max: usize) -> CodecError {
    CodecError::Limit { field, actual, max }
}
pub(super) fn encode_hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(D[usize::from(byte >> 4)] as char);
        out.push(D[usize::from(byte & 15)] as char);
    }
    out
}
