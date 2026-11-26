//! Canonical outer attempt-transaction intents.

use serde_json::Value;

use super::{
    codec::*,
    decoder::{decode_fixed_hex, preparse_bounded_tagged_row, small_unsigned, timestamp, unsigned},
    rewind::RewindMutationId,
};

pub(super) const INTENT_BYTES: [(usize, usize); 5] = [
    (968, 1_024),
    (936, 1_024),
    (936, 1_024),
    (962, 1_024),
    (310, 1_024),
];
pub(super) const REGISTER_REWIND_REF_EXACT_BYTES: usize = INTENT_BYTES[0].0;
pub(super) const SUPERSEDE_REWIND_REFS_EXACT_BYTES: usize = INTENT_BYTES[1].0;
pub(super) const RELEASE_REWIND_REFS_EXACT_BYTES: usize = INTENT_BYTES[2].0;
pub(super) const RELOCATION_HANDOFF_EXACT_BYTES: usize = INTENT_BYTES[3].0;
pub(super) const COMPACT_AGENT_INPUTS_EXACT_BYTES: usize = INTENT_BYTES[4].0;
pub(super) const INTENT_ALIGNED_BYTES: usize = INTENT_BYTES[0].1;
pub(super) const INTENT_PAIR_EXACT_BYTES: u64 = 1_936;
pub(super) const INTENT_PAIR_ALIGNED_BYTES: u64 = 2_048;

macro_rules! closed_enum {
    ($name:ident, $field:literal, {$($variant:ident = $ordinal:literal),+ $(,)?}) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub(super) enum $name { $($variant = $ordinal),+ }
        impl $name {
            pub(super) fn try_from_ordinal(value: u8) -> Result<Self> {
                match value {
                    $($ordinal => Ok(Self::$variant),)+
                    _ => Err(CodecError::Invalid($field)),
                }
            }
        }
    };
}
closed_enum!(AttemptTransactionTagV1, "attempt transaction tag", {
    RegisterRewindRef = 0,
    SupersedeRewindRefs = 1,
    ReleaseRewindRefs = 2,
    RelocationHandoff = 3,
    CompactAgentInputs = 4,
});
closed_enum!(AttemptTransactionPhaseV1, "attempt transaction phase", {
    Prepared = 0,
    SubordinatePrepared = 1,
    CommitObserved = 2,
    ProjectionsCommitted = 3,
});
closed_enum!(AttemptTransactionOperationV1, "attempt transaction operation", {
    RegisterRewindRef = 0,
    SupersedeTruncate = 1,
    SupersedeMerge = 2,
    ReleaseRewindRefs = 3,
    RelocationHandoff = 4,
    CompactAgentInputs = 5,
});
closed_enum!(AttemptTransactionTempV1, "attempt transaction temp", {
    None = 0,
    CompactAgentInputs = 1,
});

macro_rules! domains {
    ($($name:ident: $marker:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(super) enum $marker {}
        pub(super) type $name = DomainBytes<32, $marker>;
    )+};
}
domains! {
    RegisteredRewindRefHash: RegisteredRewindRef,
    SupersededRewindSetHash: SupersededRewindSet,
    ReleasedRewindSetHash: ReleasedRewindSet,
    RelocationLocatorSetHash: RelocationLocatorSet,
    ChildRewindFileHash: ChildRewindFile,
    RewindAuthoritySummaryHash: RewindAuthoritySummary,
    AttemptRowSetHash: AttemptRowSet,
    MutationCommitRowHash: MutationCommitRow,
    RewindCheckpointRoot: RewindCheckpoint,
    AttemptJournalHash: AttemptJournal,
}

macro_rules! scalar {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(super) struct $name(u64);
        impl $name {
            pub(super) fn new(value: u64) -> Self {
                Self(value)
            }
            fn value(self) -> u64 {
                self.0
            }
        }
    };
}
scalar!(CanonicalRewindRowLen);
scalar!(ChildRewindFileLen);
scalar!(MutationLedgerSequence);
scalar!(RewindCheckpointGeneration);
scalar!(LocatorRevision);
scalar!(JournalCheckpointGeneration);
scalar!(AttemptJournalLen);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RewindIntentProof<K> {
    pub(super) operation: AttemptTransactionOperationV1,
    pub(super) temp: AttemptTransactionTempV1,
    pub(super) mutation: RewindMutationId,
    pub(super) subject_hash: DomainBytes<32, K>,
    pub(super) base_file_len: ChildRewindFileLen,
    pub(super) base_file_hash: ChildRewindFileHash,
    pub(super) result_file_len: ChildRewindFileLen,
    pub(super) result_file_hash: ChildRewindFileHash,
    pub(super) base_authority_hash: RewindAuthoritySummaryHash,
    pub(super) result_authority_hash: RewindAuthoritySummaryHash,
    pub(super) base_attempt_rows_hash: AttemptRowSetHash,
    pub(super) result_attempt_rows_hash: AttemptRowSetHash,
    pub(super) ledger_sequence: MutationLedgerSequence,
    pub(super) commit_row_hash: MutationCommitRowHash,
    pub(super) base_checkpoint_generation: RewindCheckpointGeneration,
    pub(super) base_checkpoint_root: RewindCheckpointRoot,
    pub(super) result_checkpoint_generation: RewindCheckpointGeneration,
    pub(super) result_checkpoint_root: RewindCheckpointRoot,
}

macro_rules! rewind_intent {
    ($name:ident, $hash:ty) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub(super) struct $name {
            pub(super) phase: AttemptTransactionPhaseV1,
            pub(super) proof: RewindIntentProof<$hash>,
            pub(super) timestamp: Timestamp,
        }
    };
}
rewind_intent!(SupersedeRewindRefsIntent, SupersededRewindSet);
rewind_intent!(ReleaseRewindRefsIntent, ReleasedRewindSet);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegisterRewindRefIntent {
    pub(super) phase: AttemptTransactionPhaseV1,
    pub(super) proof: RewindIntentProof<RegisteredRewindRef>,
    pub(super) generation: SegmentGeneration,
    pub(super) row_len: CanonicalRewindRowLen,
    pub(super) timestamp: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RelocationHandoffIntent {
    pub(super) phase: AttemptTransactionPhaseV1,
    pub(super) proof: RewindIntentProof<RelocationLocatorSet>,
    pub(super) locator_revision: LocatorRevision,
    pub(super) timestamp: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompactAgentInputsIntent {
    pub(super) phase: AttemptTransactionPhaseV1,
    pub(super) operation: AttemptTransactionOperationV1,
    pub(super) temp: AttemptTransactionTempV1,
    pub(super) mutation: RewindMutationId,
    pub(super) base_len: AttemptJournalLen,
    pub(super) base_hash: AttemptJournalHash,
    pub(super) result_len: AttemptJournalLen,
    pub(super) result_hash: AttemptJournalHash,
    pub(super) checkpoint_generation: JournalCheckpointGeneration,
    pub(super) timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AttemptTransactionIntentV1 {
    RegisterRewindRef(RegisterRewindRefIntent),
    SupersedeRewindRefs(SupersedeRewindRefsIntent),
    ReleaseRewindRefs(ReleaseRewindRefsIntent),
    RelocationHandoff(RelocationHandoffIntent),
    CompactAgentInputs(CompactAgentInputsIntent),
}
impl AttemptTransactionIntentV1 {
    fn tag(&self) -> AttemptTransactionTagV1 {
        match self {
            Self::RegisterRewindRef(_) => AttemptTransactionTagV1::RegisterRewindRef,
            Self::SupersedeRewindRefs(_) => AttemptTransactionTagV1::SupersedeRewindRefs,
            Self::ReleaseRewindRefs(_) => AttemptTransactionTagV1::ReleaseRewindRefs,
            Self::RelocationHandoff(_) => AttemptTransactionTagV1::RelocationHandoff,
            Self::CompactAgentInputs(_) => AttemptTransactionTagV1::CompactAgentInputs,
        }
    }
    fn limits(&self) -> (usize, usize) {
        INTENT_BYTES[usize::from(self.tag() as u8)]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EncodedAttemptTransactionIntent(Vec<u8>);
impl EncodedAttemptTransactionIntent {
    pub(super) fn try_new(intent: &AttemptTransactionIntentV1) -> Result<Self> {
        validate(intent)?;
        let mut bytes = serde_json::to_vec(&wire(intent)).map_err(|_| CodecError::Json)?;
        bytes.push(b'\n');
        let (max, _) = intent.limits();
        if bytes.len() > max {
            return Err(CodecError::Limit {
                field: "encoded intent",
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

fn validate_proof<K>(proof: &RewindIntentProof<K>) -> Result<()> {
    if proof.temp != AttemptTransactionTempV1::None
        || proof.base_checkpoint_generation.value().checked_add(1)
            != Some(proof.result_checkpoint_generation.value())
    {
        return Err(CodecError::Invalid("attempt transaction proof"));
    }
    Ok(())
}
fn validate_operation(
    actual: AttemptTransactionOperationV1,
    expected: AttemptTransactionOperationV1,
) -> Result<()> {
    if actual != expected {
        return Err(CodecError::Invalid("attempt transaction operation"));
    }
    Ok(())
}
fn validate(intent: &AttemptTransactionIntentV1) -> Result<()> {
    match intent {
        AttemptTransactionIntentV1::RegisterRewindRef(value) => {
            validate_proof(&value.proof)?;
            validate_operation(
                value.proof.operation,
                AttemptTransactionOperationV1::RegisterRewindRef,
            )
        }
        AttemptTransactionIntentV1::SupersedeRewindRefs(value) => {
            validate_proof(&value.proof)?;
            if !matches!(
                value.proof.operation,
                AttemptTransactionOperationV1::SupersedeTruncate
                    | AttemptTransactionOperationV1::SupersedeMerge
            ) {
                return Err(CodecError::Invalid("attempt transaction operation"));
            }
            Ok(())
        }
        AttemptTransactionIntentV1::ReleaseRewindRefs(value) => {
            validate_proof(&value.proof)?;
            validate_operation(
                value.proof.operation,
                AttemptTransactionOperationV1::ReleaseRewindRefs,
            )
        }
        AttemptTransactionIntentV1::RelocationHandoff(value) => {
            validate_proof(&value.proof)?;
            validate_operation(
                value.proof.operation,
                AttemptTransactionOperationV1::RelocationHandoff,
            )
        }
        AttemptTransactionIntentV1::CompactAgentInputs(value) => {
            validate_operation(
                value.operation,
                AttemptTransactionOperationV1::CompactAgentInputs,
            )?;
            if value.temp != AttemptTransactionTempV1::CompactAgentInputs {
                return Err(CodecError::Invalid("attempt transaction temp"));
            }
            Ok(())
        }
    }
}

fn wire(intent: &AttemptTransactionIntentV1) -> Value {
    let h = |value: &[u8]| encode_hex(value);
    match intent {
        AttemptTransactionIntentV1::RegisterRewindRef(v) => {
            let p = &v.proof;
            serde_json::json!({"v":1,"k":0,"p":v.phase as u8,"o":p.operation as u8,"s":p.temp as u8,"m":h(p.mutation.as_bytes()),"g":v.generation.value(),"x":h(p.subject_hash.as_bytes()),"z":v.row_len.value(),"bl":p.base_file_len.value(),"bh":h(p.base_file_hash.as_bytes()),"rl":p.result_file_len.value(),"rh":h(p.result_file_hash.as_bytes()),"ba":h(p.base_authority_hash.as_bytes()),"ra":h(p.result_authority_hash.as_bytes()),"bj":h(p.base_attempt_rows_hash.as_bytes()),"rj":h(p.result_attempt_rows_hash.as_bytes()),"q":p.ledger_sequence.value(),"c":h(p.commit_row_hash.as_bytes()),"bg":p.base_checkpoint_generation.value(),"br":h(p.base_checkpoint_root.as_bytes()),"rg":p.result_checkpoint_generation.value(),"rr":h(p.result_checkpoint_root.as_bytes()),"t":v.timestamp.value()})
        }
        AttemptTransactionIntentV1::SupersedeRewindRefs(v) => {
            rewind_wire(1, v.phase, &v.proof, None, v.timestamp)
        }
        AttemptTransactionIntentV1::ReleaseRewindRefs(v) => {
            rewind_wire(2, v.phase, &v.proof, None, v.timestamp)
        }
        AttemptTransactionIntentV1::RelocationHandoff(v) => rewind_wire(
            3,
            v.phase,
            &v.proof,
            Some(v.locator_revision.value()),
            v.timestamp,
        ),
        AttemptTransactionIntentV1::CompactAgentInputs(v) => {
            serde_json::json!({"v":1,"k":4,"p":v.phase as u8,"o":v.operation as u8,"s":v.temp as u8,"m":h(v.mutation.as_bytes()),"bl":v.base_len.value(),"bh":h(v.base_hash.as_bytes()),"rl":v.result_len.value(),"rh":h(v.result_hash.as_bytes()),"g":v.checkpoint_generation.value(),"t":v.timestamp.value()})
        }
    }
}
fn rewind_wire<K>(
    tag: u8,
    phase: AttemptTransactionPhaseV1,
    p: &RewindIntentProof<K>,
    locator: Option<u64>,
    timestamp: Timestamp,
) -> Value {
    let h = |value: &[u8]| encode_hex(value);
    if let Some(locator) = locator {
        serde_json::json!({"v":1,"k":tag,"p":phase as u8,"o":p.operation as u8,"s":p.temp as u8,"m":h(p.mutation.as_bytes()),"x":h(p.subject_hash.as_bytes()),"bl":p.base_file_len.value(),"bh":h(p.base_file_hash.as_bytes()),"rl":p.result_file_len.value(),"rh":h(p.result_file_hash.as_bytes()),"ba":h(p.base_authority_hash.as_bytes()),"ra":h(p.result_authority_hash.as_bytes()),"bj":h(p.base_attempt_rows_hash.as_bytes()),"rj":h(p.result_attempt_rows_hash.as_bytes()),"q":p.ledger_sequence.value(),"c":h(p.commit_row_hash.as_bytes()),"bg":p.base_checkpoint_generation.value(),"br":h(p.base_checkpoint_root.as_bytes()),"rg":p.result_checkpoint_generation.value(),"rr":h(p.result_checkpoint_root.as_bytes()),"lr":locator,"t":timestamp.value()})
    } else {
        serde_json::json!({"v":1,"k":tag,"p":phase as u8,"o":p.operation as u8,"s":p.temp as u8,"m":h(p.mutation.as_bytes()),"x":h(p.subject_hash.as_bytes()),"bl":p.base_file_len.value(),"bh":h(p.base_file_hash.as_bytes()),"rl":p.result_file_len.value(),"rh":h(p.result_file_hash.as_bytes()),"ba":h(p.base_authority_hash.as_bytes()),"ra":h(p.result_authority_hash.as_bytes()),"bj":h(p.base_attempt_rows_hash.as_bytes()),"rj":h(p.result_attempt_rows_hash.as_bytes()),"q":p.ledger_sequence.value(),"c":h(p.commit_row_hash.as_bytes()),"bg":p.base_checkpoint_generation.value(),"br":h(p.base_checkpoint_root.as_bytes()),"rg":p.result_checkpoint_generation.value(),"rr":h(p.result_checkpoint_root.as_bytes()),"t":timestamp.value()})
    }
}

pub(super) fn decode_attempt_transaction_intent(
    bytes: &[u8],
) -> Result<AttemptTransactionIntentV1> {
    let row = preparse_bounded_tagged_row(bytes, INTENT_ALIGNED_BYTES, |tag| {
        INTENT_BYTES.get(usize::from(tag)).map(|limits| limits.0)
    })?;
    let fields = &row.fields;
    let phase = AttemptTransactionPhaseV1::try_from_ordinal(small_unsigned(
        fields,
        "p",
        "attempt transaction phase",
    )?)?;
    let operation = AttemptTransactionOperationV1::try_from_ordinal(small_unsigned(
        fields,
        "o",
        "attempt transaction operation",
    )?)?;
    let temp = AttemptTransactionTempV1::try_from_ordinal(small_unsigned(
        fields,
        "s",
        "attempt transaction temp",
    )?)?;
    let record = match AttemptTransactionTagV1::try_from_ordinal(row.event)? {
        AttemptTransactionTagV1::RegisterRewindRef => {
            AttemptTransactionIntentV1::RegisterRewindRef(RegisterRewindRefIntent {
                phase,
                proof: decode_proof(fields, operation, temp)?,
                generation: SegmentGeneration::try_new(small_unsigned(
                    fields,
                    "g",
                    "segment generation",
                )?)?,
                row_len: CanonicalRewindRowLen::new(unsigned(
                    fields,
                    "z",
                    "canonical rewind row length",
                )?),
                timestamp: timestamp(fields)?,
            })
        }
        AttemptTransactionTagV1::SupersedeRewindRefs => {
            AttemptTransactionIntentV1::SupersedeRewindRefs(SupersedeRewindRefsIntent {
                phase,
                proof: decode_proof(fields, operation, temp)?,
                timestamp: timestamp(fields)?,
            })
        }
        AttemptTransactionTagV1::ReleaseRewindRefs => {
            AttemptTransactionIntentV1::ReleaseRewindRefs(ReleaseRewindRefsIntent {
                phase,
                proof: decode_proof(fields, operation, temp)?,
                timestamp: timestamp(fields)?,
            })
        }
        AttemptTransactionTagV1::RelocationHandoff => {
            AttemptTransactionIntentV1::RelocationHandoff(RelocationHandoffIntent {
                phase,
                proof: decode_proof(fields, operation, temp)?,
                locator_revision: LocatorRevision::new(unsigned(fields, "lr", "locator revision")?),
                timestamp: timestamp(fields)?,
            })
        }
        AttemptTransactionTagV1::CompactAgentInputs => {
            AttemptTransactionIntentV1::CompactAgentInputs(CompactAgentInputsIntent {
                phase,
                operation,
                temp,
                mutation: RewindMutationId::new(decode_fixed_hex(
                    fields,
                    "m",
                    "rewind mutation ID",
                )?),
                base_len: AttemptJournalLen::new(unsigned(fields, "bl", "base journal length")?),
                base_hash: DomainBytes::new(decode_fixed_hex(fields, "bh", "base journal hash")?),
                result_len: AttemptJournalLen::new(unsigned(
                    fields,
                    "rl",
                    "result journal length",
                )?),
                result_hash: DomainBytes::new(decode_fixed_hex(
                    fields,
                    "rh",
                    "result journal hash",
                )?),
                checkpoint_generation: JournalCheckpointGeneration::new(unsigned(
                    fields,
                    "g",
                    "journal checkpoint generation",
                )?),
                timestamp: timestamp(fields)?,
            })
        }
    };
    if EncodedAttemptTransactionIntent::try_new(&record)?.as_bytes() != bytes {
        return Err(CodecError::Invalid("canonical attempt transaction intent"));
    }
    Ok(record)
}

fn decode_proof<K>(
    fields: &serde_json::Map<String, Value>,
    operation: AttemptTransactionOperationV1,
    temp: AttemptTransactionTempV1,
) -> Result<RewindIntentProof<K>> {
    Ok(RewindIntentProof {
        operation,
        temp,
        mutation: RewindMutationId::new(decode_fixed_hex(fields, "m", "rewind mutation ID")?),
        subject_hash: DomainBytes::new(decode_fixed_hex(
            fields,
            "x",
            "attempt transaction subject hash",
        )?),
        base_file_len: ChildRewindFileLen::new(unsigned(fields, "bl", "base child file length")?),
        base_file_hash: DomainBytes::new(decode_fixed_hex(fields, "bh", "base child file hash")?),
        result_file_len: ChildRewindFileLen::new(unsigned(
            fields,
            "rl",
            "result child file length",
        )?),
        result_file_hash: DomainBytes::new(decode_fixed_hex(
            fields,
            "rh",
            "result child file hash",
        )?),
        base_authority_hash: DomainBytes::new(decode_fixed_hex(
            fields,
            "ba",
            "base authority summary hash",
        )?),
        result_authority_hash: DomainBytes::new(decode_fixed_hex(
            fields,
            "ra",
            "result authority summary hash",
        )?),
        base_attempt_rows_hash: DomainBytes::new(decode_fixed_hex(
            fields,
            "bj",
            "base attempt row-set hash",
        )?),
        result_attempt_rows_hash: DomainBytes::new(decode_fixed_hex(
            fields,
            "rj",
            "result attempt row-set hash",
        )?),
        ledger_sequence: MutationLedgerSequence::new(unsigned(
            fields,
            "q",
            "mutation ledger sequence",
        )?),
        commit_row_hash: DomainBytes::new(decode_fixed_hex(
            fields,
            "c",
            "mutation commit-row hash",
        )?),
        base_checkpoint_generation: RewindCheckpointGeneration::new(unsigned(
            fields,
            "bg",
            "base checkpoint generation",
        )?),
        base_checkpoint_root: DomainBytes::new(decode_fixed_hex(
            fields,
            "br",
            "base checkpoint root",
        )?),
        result_checkpoint_generation: RewindCheckpointGeneration::new(unsigned(
            fields,
            "rg",
            "result checkpoint generation",
        )?),
        result_checkpoint_root: DomainBytes::new(decode_fixed_hex(
            fields,
            "rr",
            "result checkpoint root",
        )?),
    })
}
