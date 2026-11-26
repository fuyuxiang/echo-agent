//! Canonical rewind-reference and journal-checkpoint records.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::codec::*;

const FIRST_EVENT: u8 = 11;
pub(super) const A2_EXACT_ALIGNED_ROW_BYTES: [(usize, usize); 5] =
    [(200, 224), (292, 320), (129, 160), (129, 160), (176, 192)];

macro_rules! domains {
    ($($name:ident: $marker:ident[$width:expr]),+ $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub(super) enum $marker {}
        pub(super) type $name = DomainBytes<$width, $marker>;
    )+};
}
domains! {
    ChildStorageKey: ChildStorage[16], RewindRowHash: RewindRow[32], RemovedRefHash: RemovedRef[32],
    RewindMutationId: RewindMutation[16], PreRemovalAncestryRoot: PreRemovalAncestry[32],
    CanonicalRewindFileHash: CanonicalRewindFile[32], AuthoritySetHash: AuthoritySet[32],
    JournalPrefixHash: JournalPrefix[32],
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
scalar!(PromptIndex);
scalar!(SerializedRowLen);
scalar!(ReleaseGeneration);
scalar!(CheckpointGeneration);
scalar!(JournalPrefixLen);

macro_rules! count {
    ($name:ident, $max:expr, $field:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(super) struct $name(u8);
        impl $name {
            pub(super) fn try_new(value: u8) -> Result<Self> {
                if value <= $max {
                    Ok(Self(value))
                } else {
                    Err(CodecError::Invalid($field))
                }
            }
            fn value(self) -> u8 {
                self.0
            }
        }
    };
}
count!(AcceptedRowCount, 32, "accepted row count");
count!(ControlRowCount, 238, "control row count");
count!(SupersessionRowCount, 33, "supersession row count");

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RewindRefLiveRecord {
    pub(super) generation: SegmentGeneration,
    pub(super) child_storage: ChildStorageKey,
    pub(super) prompt_index: PromptIndex,
    pub(super) serialized_row_len: SerializedRowLen,
    pub(super) row_hash: RewindRowHash,
    pub(super) timestamp: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RewindRefSupersededRecord {
    pub(super) generation: SegmentGeneration,
    pub(super) removed_ref_hash: RemovedRefHash,
    pub(super) removing_mutation: RewindMutationId,
    pub(super) pre_removal_ancestry: PreRemovalAncestryRoot,
    pub(super) resulting_file_hash: CanonicalRewindFileHash,
    pub(super) timestamp: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RewindRefReleaseRecord {
    pub(super) generation: ReleaseGeneration,
    pub(super) authority_set_hash: AuthoritySetHash,
    pub(super) timestamp: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JournalCheckpointRecord {
    pub(super) generation: CheckpointGeneration,
    pub(super) prefix_len: JournalPrefixLen,
    pub(super) prefix_hash: JournalPrefixHash,
    pub(super) accepted_rows: AcceptedRowCount,
    pub(super) control_rows: ControlRowCount,
    pub(super) supersession_rows: SupersessionRowCount,
    pub(super) timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RewindRecordV1 {
    Live(RewindRefLiveRecord),
    Superseded(RewindRefSupersededRecord),
    ReleaseIntent(RewindRefReleaseRecord),
    ReleaseReceipt(RewindRefReleaseRecord),
    Checkpoint(JournalCheckpointRecord),
}
impl RewindRecordV1 {
    pub(super) fn limits(&self) -> (usize, usize) {
        A2_EXACT_ALIGNED_ROW_BYTES[usize::from(self.event() - FIRST_EVENT)]
    }
    fn event(&self) -> u8 {
        match self {
            Self::Live(_) => 11,
            Self::Superseded(_) => 12,
            Self::ReleaseIntent(_) => 13,
            Self::ReleaseReceipt(_) => 14,
            Self::Checkpoint(_) => 15,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EncodedRewindRecord(Vec<u8>);
impl EncodedRewindRecord {
    pub(super) fn try_new(record: &RewindRecordV1) -> Result<Self> {
        let mut bytes = serde_json::to_vec(&wire(record)).map_err(|_| CodecError::Json)?;
        bytes.push(b'\n');
        let (_, cap) = record.limits();
        if bytes.len() > cap {
            return Err(CodecError::Limit {
                field: "encoded rewind record",
                actual: bytes.len(),
                max: cap,
            });
        }
        Ok(Self(bytes))
    }
    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

fn wire(record: &RewindRecordV1) -> Value {
    let x = |value: &[u8]| encode_hex(value);
    match record {
        RewindRecordV1::Live(v) => {
            json!({"v":1,"e":11,"g":v.generation.value(),"s":x(v.child_storage.as_bytes()),"p":v.prompt_index.value(),"l":v.serialized_row_len.value(),"w":x(v.row_hash.as_bytes()),"t":v.timestamp.value()})
        }
        RewindRecordV1::Superseded(v) => {
            json!({"v":1,"e":12,"g":v.generation.value(),"h":x(v.removed_ref_hash.as_bytes()),"m":x(v.removing_mutation.as_bytes()),"a":x(v.pre_removal_ancestry.as_bytes()),"f":x(v.resulting_file_hash.as_bytes()),"t":v.timestamp.value()})
        }
        RewindRecordV1::ReleaseIntent(v) => {
            json!({"v":1,"e":13,"g":v.generation.value(),"h":x(v.authority_set_hash.as_bytes()),"t":v.timestamp.value()})
        }
        RewindRecordV1::ReleaseReceipt(v) => {
            json!({"v":1,"e":14,"g":v.generation.value(),"h":x(v.authority_set_hash.as_bytes()),"t":v.timestamp.value()})
        }
        RewindRecordV1::Checkpoint(v) => {
            json!({"v":1,"e":15,"q":v.generation.value(),"l":v.prefix_len.value(),"h":x(v.prefix_hash.as_bytes()),"a":v.accepted_rows.value(),"c":v.control_rows.value(),"s":v.supersession_rows.value(),"t":v.timestamp.value()})
        }
    }
}

pub(super) fn decode_rewind_record(
    event: u8,
    unsigned: &dyn Fn(&str, &'static str) -> Result<u64>,
    small_unsigned: &dyn Fn(&str, &'static str) -> Result<u8>,
    fixed_hex_16: &dyn Fn(&str, &'static str) -> Result<[u8; 16]>,
    fixed_hex_32: &dyn Fn(&str, &'static str) -> Result<[u8; 32]>,
    timestamp: &dyn Fn() -> Result<Timestamp>,
) -> Result<RewindRecordV1> {
    let record = match event {
        11 => RewindRecordV1::Live(RewindRefLiveRecord {
            generation: SegmentGeneration::try_new(small_unsigned("g", "segment generation")?)?,
            child_storage: ChildStorageKey::new(fixed_hex_16("s", "child storage key")?),
            prompt_index: PromptIndex::new(unsigned("p", "prompt index")?),
            serialized_row_len: SerializedRowLen::new(unsigned("l", "serialized row length")?),
            row_hash: DomainBytes::new(fixed_hex_32("w", "rewind row hash")?),
            timestamp: timestamp()?,
        }),
        12 => RewindRecordV1::Superseded(RewindRefSupersededRecord {
            generation: SegmentGeneration::try_new(small_unsigned("g", "segment generation")?)?,
            removed_ref_hash: DomainBytes::new(fixed_hex_32("h", "removed ref hash")?),
            removing_mutation: RewindMutationId::new(fixed_hex_16("m", "removing mutation ID")?),
            pre_removal_ancestry: DomainBytes::new(fixed_hex_32("a", "pre-removal ancestry root")?),
            resulting_file_hash: DomainBytes::new(fixed_hex_32("f", "resulting file hash")?),
            timestamp: timestamp()?,
        }),
        13 | 14 => {
            let value = RewindRefReleaseRecord {
                generation: ReleaseGeneration::new(unsigned("g", "release generation")?),
                authority_set_hash: DomainBytes::new(fixed_hex_32("h", "authority set hash")?),
                timestamp: timestamp()?,
            };
            if event == 13 {
                RewindRecordV1::ReleaseIntent(value)
            } else {
                RewindRecordV1::ReleaseReceipt(value)
            }
        }
        15 => RewindRecordV1::Checkpoint(JournalCheckpointRecord {
            generation: CheckpointGeneration::new(unsigned("q", "checkpoint generation")?),
            prefix_len: JournalPrefixLen::new(unsigned("l", "journal prefix length")?),
            prefix_hash: DomainBytes::new(fixed_hex_32("h", "journal prefix hash")?),
            accepted_rows: AcceptedRowCount::try_new(small_unsigned("a", "accepted row count")?)?,
            control_rows: ControlRowCount::try_new(small_unsigned("c", "control row count")?)?,
            supersession_rows: SupersessionRowCount::try_new(small_unsigned(
                "s",
                "supersession row count",
            )?)?,
            timestamp: timestamp()?,
        }),
        _ => return Err(CodecError::Invalid("record event")),
    };
    Ok(record)
}

pub(super) fn rewind_row_hash(row: &[u8]) -> Result<RewindRowHash> {
    if !row.ends_with(b"\n") || row[..row.len() - 1].contains(&b'\n') || row.contains(&b'\r') {
        return Err(CodecError::Invalid("rewind row line ending"));
    }
    Ok(RewindRowHash::new(Sha256::digest(row).into()))
}
