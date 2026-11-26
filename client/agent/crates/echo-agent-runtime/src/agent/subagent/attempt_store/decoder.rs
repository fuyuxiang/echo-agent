//! Allocation-bounded strict decoding for canonical attempt records.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::{codec::*, rewind::*};

const PREFIX: &[u8] = b"{\"v\":1,\"e\":";
const MAX_CONTENT_ENCODED_BYTES: usize = (MAX_MESSAGE_RAW_BYTES * 4).div_ceil(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DecodedAttemptRecordV1 {
    Core(RecordV1),
    Rewind(RewindRecordV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordFamilyV1 {
    Attempt,
    Completion,
    Recovery,
}
impl RecordFamilyV1 {
    fn encoded_field(self) -> &'static str {
        match self {
            Self::Attempt => "encoded record",
            Self::Completion => "encoded completion record",
            Self::Recovery => "encoded recovery record",
        }
    }
    fn invalid(
        self,
        attempt: &'static str,
        completion: &'static str,
        recovery: &'static str,
    ) -> CodecError {
        CodecError::Invalid(match self {
            Self::Attempt => attempt,
            Self::Completion => completion,
            Self::Recovery => recovery,
        })
    }
}

pub(super) struct BoundedRowV1 {
    pub(super) event: u8,
    pub(super) fields: Map<String, Value>,
}

pub(super) fn preparse_bounded_tagged_row(
    bytes: &[u8],
    global_cap: usize,
    tag_cap: impl Fn(u8) -> Option<usize>,
) -> Result<BoundedRowV1> {
    check_bounded_cap(bytes, global_cap, "encoded intent")?;
    let body = bytes
        .strip_suffix(b"\n")
        .filter(|body| !body.contains(&b'\n') && !body.contains(&b'\r'))
        .ok_or(CodecError::Invalid("intent line ending"))?;
    let after = body
        .strip_prefix(b"{\"v\":1,\"k\":")
        .ok_or(CodecError::Invalid("intent dispatch"))?;
    let tag = match after {
        [digit @ b'0'..=b'9', b',', ..] => digit - b'0',
        _ => return Err(CodecError::Invalid("attempt transaction tag")),
    };
    let max = tag_cap(tag).ok_or(CodecError::Invalid("attempt transaction tag"))?;
    check_bounded_cap(bytes, max, "encoded intent")?;
    let fields: Map<String, Value> = serde_json::from_slice(body).map_err(|_| CodecError::Json)?;
    if unsigned(&fields, "v", "version")? != 1
        || unsigned(&fields, "k", "attempt transaction tag")? != u64::from(tag)
    {
        return Err(CodecError::Invalid("intent dispatch"));
    }
    Ok(BoundedRowV1 { event: tag, fields })
}

pub(super) fn preparse_bounded_row(
    bytes: &[u8],
    family: RecordFamilyV1,
    global_cap: usize,
    event_cap: impl Fn(u8) -> Option<usize>,
    before_json: impl FnOnce(u8, &[u8]) -> Result<()>,
) -> Result<BoundedRowV1> {
    check_bounded_cap(bytes, global_cap, family.encoded_field())?;
    let body = bytes
        .strip_suffix(b"\n")
        .filter(|body| !body.contains(&b'\n') && !body.contains(&b'\r'))
        .ok_or_else(|| {
            family.invalid(
                "record line ending",
                "completion record line ending",
                "recovery record line ending",
            )
        })?;
    let after = body.strip_prefix(PREFIX).ok_or_else(|| {
        family.invalid(
            "record dispatch",
            "completion record dispatch",
            "recovery record dispatch",
        )
    })?;
    let event = match after {
        [digit @ b'0'..=b'9', b',', ..] => digit - b'0',
        [tens @ b'1'..=b'9', ones @ b'0'..=b'9', b',', ..] => (tens - b'0') * 10 + ones - b'0',
        _ => {
            return Err(family.invalid("record event", "completion event", "recovery event"));
        }
    };
    let max = event_cap(event)
        .ok_or_else(|| family.invalid("record event", "completion event", "recovery event"))?;
    check_bounded_cap(bytes, max, family.encoded_field())?;
    before_json(event, body)?;
    let fields: Map<String, Value> = serde_json::from_slice(body).map_err(|_| CodecError::Json)?;
    if unsigned(&fields, "v", "version")? != 1
        || unsigned(&fields, "e", "event")? != u64::from(event)
    {
        return Err(family.invalid(
            "record dispatch",
            "completion record dispatch",
            "recovery record dispatch",
        ));
    }
    Ok(BoundedRowV1 { event, fields })
}

#[allow(
    dead_code,
    reason = "decoder foundation is consumed by the next storage slice"
)]
pub(super) fn decode_attempt_record(bytes: &[u8]) -> Result<DecodedAttemptRecordV1> {
    let row = preparse_bounded_row(
        bytes,
        RecordFamilyV1::Attempt,
        MAX_ENCODED_RECORD_BYTES,
        |event| match event {
            0..=10 => Some(ROW_LIMITS[usize::from(event)].1),
            11..=15 => Some(A2_EXACT_ALIGNED_ROW_BYTES[usize::from(event - 11)].1),
            _ => None,
        },
        |event, body| {
            if event == 2 {
                check_content_encoded_len(body)?;
            }
            Ok(())
        },
    )?;
    let event = row.event;
    let fields: DecodedFields<'_> = &row.fields;
    if event <= 10 {
        let record = decode_core_record(event, fields)?;
        if EncodedRecord::try_new(&record)?.as_bytes() != bytes {
            return Err(CodecError::Invalid("canonical record"));
        }
        Ok(DecodedAttemptRecordV1::Core(record))
    } else {
        let record = decode_rewind_record(
            event,
            &|key, field| unsigned(fields, key, field),
            &|key, field| small_unsigned(fields, key, field),
            &|key, field| decode_fixed_hex(fields, key, field),
            &|key, field| decode_fixed_hex(fields, key, field),
            &|| timestamp(fields),
        )?;
        if EncodedRewindRecord::try_new(&record)?.as_bytes() != bytes {
            return Err(CodecError::Invalid("canonical record"));
        }
        Ok(DecodedAttemptRecordV1::Rewind(record))
    }
}

fn decode_core_record(event: u8, fields: DecodedFields<'_>) -> Result<RecordV1> {
    let object = fields;
    let record = match event {
        0 => RecordV1::AttemptHeader(AttemptHeaderRecord {
            attempt: fixed_hex(object, "a", "attempt ID")?,
            owner_root: fixed_hex(object, "r", "ownership root ID")?,
            generation: AttemptGeneration::try_new(unsigned(object, "g", "attempt generation")?)?,
            initial: SegmentKindV1::try_from_ordinal(small_unsigned(object, "k", "segment kind")?)?,
            constructor_digest: fixed_hex(object, "h", "constructor digest")?,
            timestamp: timestamp(object)?,
        }),
        1 => RecordV1::CapacityReserved(CapacityReservedRecord {
            profile: CapacityProfile {
                segments: small_unsigned(object, "s", "segments")?,
                messages: small_unsigned(object, "m", "messages")?,
                raw_bytes: unsigned(object, "b", "raw bytes")?,
                content_bytes: unsigned(object, "c", "content bytes")?,
                logical_bytes: unsigned(object, "l", "logical bytes")?,
                physical_bytes: unsigned(object, "p", "physical bytes")?,
                directory_bytes: unsigned(object, "d", "directory bytes")?,
            },
            timestamp: timestamp(object)?,
        }),
        2 => {
            let encoded = string(object, "c", "agent text")?;
            if encoded.len() > MAX_CONTENT_ENCODED_BYTES {
                return Err(CodecError::Limit {
                    field: "encoded agent text",
                    actual: encoded.len(),
                    max: MAX_CONTENT_ENCODED_BYTES,
                });
            }
            let decoded = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| CodecError::Invalid("agent text base64url"))?;
            if (decoded.len() * 4).div_ceil(3) != encoded.len() {
                return Err(CodecError::Invalid("agent text base64url"));
            }
            RecordV1::AcceptedAgentContent(AcceptedAgentContentRecord {
                generation: segment_generation(object)?,
                kind: SegmentKindV1::try_from_ordinal(small_unsigned(
                    object,
                    "k",
                    "segment kind",
                )?)?,
                message: fixed_hex(object, "m", "agent message ID")?,
                sender_session: fixed_hex(object, "s", "sender session ID")?,
                source_attempt: optional_fixed_hex(object, "x", "source attempt ID")?,
                relation: AgentSenderRelationV1::try_from_ordinal(small_unsigned(
                    object,
                    "r",
                    "sender relation",
                )?)?,
                authority: AgentAuthorityV1::try_from_ordinal(small_unsigned(
                    object,
                    "a",
                    "agent authority",
                )?)?,
                text: AgentText::try_new(&decoded)?,
                timestamp: timestamp(object)?,
            })
        }
        3 => RecordV1::SegmentReserved(SegmentReservedRecord {
            generation: segment_generation(object)?,
            barrier: BarrierGeneration::new(unsigned(object, "b", "barrier generation")?),
            kind: SegmentKindV1::try_from_ordinal(small_unsigned(object, "k", "segment kind")?)?,
            prompt: fixed_hex(object, "p", "prompt ID")?,
            payload_hash: fixed_hex(object, "h", "payload hash")?,
            timestamp: timestamp(object)?,
        }),
        4 => RecordV1::TurnStarted(TurnStartedRecord {
            generation: segment_generation(object)?,
            timestamp: timestamp(object)?,
        }),
        5 => RecordV1::TurnCommitIntent(TurnCommitIntentRecord {
            generation: segment_generation(object)?,
            projection_hash: fixed_hex(object, "p", "projection set hash")?,
            core_hash: fixed_hex(object, "c", "completion core hash")?,
            timestamp: timestamp(object)?,
        }),
        6 => RecordV1::TurnResolution(TurnResolutionRecord {
            generation: segment_generation(object)?,
            resolution: TurnResolutionV1::try_from_ordinal(small_unsigned(
                object,
                "o",
                "turn resolution",
            )?)?,
            core_hash: fixed_hex(object, "c", "completion core hash")?,
            rewind_ref_hash: optional_fixed_hex(object, "r", "rewind ref hash")?,
            timestamp: timestamp(object)?,
        }),
        7 => RecordV1::InputDisposition(InputDispositionRecord {
            generation: segment_generation(object)?,
            disposition: InputDispositionV1::try_from_ordinals(
                small_unsigned(object, "o", "input disposition")?,
                small_unsigned(object, "r", "input disposition reason")?,
            )?,
            timestamp: timestamp(object)?,
        }),
        8 => RecordV1::QueueCut(QueueCutRecord {
            generation: segment_generation(object)?,
            queue_generation: QueueGeneration::new(unsigned(object, "q", "queue generation")?),
            timestamp: timestamp(object)?,
        }),
        9 => RecordV1::AdmissionClosed(AdmissionClosedRecord {
            barrier: BarrierGeneration::new(unsigned(object, "b", "barrier generation")?),
            reason: AdmissionCloseReasonV1::try_from_ordinal(small_unsigned(
                object,
                "o",
                "admission close reason",
            )?)?,
            timestamp: timestamp(object)?,
        }),
        10 => RecordV1::AttemptOutcome(AttemptOutcomeRecord {
            high_water: SegmentHighWater::try_new(small_unsigned(
                object,
                "w",
                "segment high water",
            )?)?,
            outcome: AttemptOutcomeV1::try_from_ordinals(
                small_unsigned(object, "o", "attempt outcome")?,
                small_unsigned(object, "r", "attempt outcome reason")?,
            )?,
            timestamp: timestamp(object)?,
        }),
        _ => return Err(CodecError::Invalid("record event")),
    };
    Ok(record)
}

// Lexically find every top-level key that decodes to `c` (including whitespace
// around `:` and `\u0063` escapes) and bound its string before JSON allocation.
fn check_content_encoded_len(body: &[u8]) -> Result<()> {
    let mut fields = body
        .strip_prefix(b"{")
        .and_then(|bytes| bytes.strip_suffix(b"}"))
        .ok_or(CodecError::Invalid("record object"))?;
    let mut seen_content = false;
    while !fields.is_empty() {
        fields = trim_start(fields);
        let (key, after_key) =
            json_string(fields).ok_or(CodecError::Invalid("agent text field"))?;
        fields = trim_start(after_key);
        fields = fields
            .strip_prefix(b":")
            .ok_or(CodecError::Invalid("agent text field"))?;
        fields = trim_start(fields);
        let (value, after_value) =
            json_value(fields).ok_or(CodecError::Invalid("agent text field"))?;
        if key_is_c(key)? {
            bound_content_string(value)?;
            if seen_content {
                return Err(CodecError::Invalid("agent text field"));
            }
            seen_content = true;
        }
        fields = trim_start(after_value);
        if fields.is_empty() {
            break;
        }
        fields = fields
            .strip_prefix(b",")
            .ok_or(CodecError::Invalid("agent text field"))?;
    }
    if !seen_content {
        return Err(CodecError::Invalid("agent text field"));
    }
    Ok(())
}

fn bound_content_string(value: &[u8]) -> Result<()> {
    let encoded = value
        .strip_prefix(b"\"")
        .and_then(|bytes| bytes.strip_suffix(b"\""))
        .ok_or(CodecError::Invalid("agent text field"))?;
    if encoded.contains(&b'\\') {
        return Err(CodecError::Invalid("agent text base64url"));
    }
    if encoded.len() > MAX_CONTENT_ENCODED_BYTES {
        return Err(CodecError::Limit {
            field: "encoded agent text",
            actual: encoded.len(),
            max: MAX_CONTENT_ENCODED_BYTES,
        });
    }
    if !encoded
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CodecError::Invalid("agent text base64url"));
    }
    Ok(())
}

fn key_is_c(key: &[u8]) -> Result<bool> {
    let mut decoded = [0_u8; 1];
    let mut out_len = 0_usize;
    let mut index = 0_usize;
    while index < key.len() {
        let (byte, consumed) = match key[index] {
            b'\\' => match key.get(index + 1) {
                Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                    let mapped = match key[index + 1] {
                        b'b' => b'\x08',
                        b'f' => b'\x0c',
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        other => other,
                    };
                    (mapped, 2)
                }
                Some(b'u') if index + 6 <= key.len() => {
                    let hex = &key[index + 2..index + 6];
                    let mut value = 0_u16;
                    for byte in hex {
                        value = (value << 4)
                            | match byte {
                                b'0'..=b'9' => u16::from(byte - b'0'),
                                b'a'..=b'f' => u16::from(byte - b'a' + 10),
                                b'A'..=b'F' => u16::from(byte - b'A' + 10),
                                _ => return Err(CodecError::Invalid("agent text field")),
                            };
                    }
                    if value > 0x7f {
                        return Ok(false);
                    }
                    (value as u8, 6)
                }
                _ => return Err(CodecError::Invalid("agent text field")),
            },
            byte if byte >= 0x20 && byte != b'"' => (byte, 1),
            _ => return Err(CodecError::Invalid("agent text field")),
        };
        if out_len >= decoded.len() {
            return Ok(false);
        }
        decoded[out_len] = byte;
        out_len += 1;
        index += consumed;
    }
    Ok(out_len == 1 && decoded[0] == b'c')
}

fn json_string(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = input.strip_prefix(b"\"")?;
    let mut index = 0_usize;
    while index < rest.len() {
        match rest[index] {
            b'"' => return Some((&rest[..index], &rest[index + 1..])),
            b'\\' => {
                index += 1;
                if index >= rest.len() {
                    return None;
                }
                if rest[index] == b'u' {
                    index = index.checked_add(4)?;
                }
                index += 1;
            }
            byte if byte < 0x20 => return None,
            _ => index += 1,
        }
    }
    None
}

fn json_value(input: &[u8]) -> Option<(&[u8], &[u8])> {
    match input.first()? {
        b'"' => {
            let (interior, rest) = json_string(input)?;
            let value_len = interior.len() + 2;
            Some((&input[..value_len], rest))
        }
        b'{' => json_container(input, b'{', b'}'),
        b'[' => json_container(input, b'[', b']'),
        b't' if input.starts_with(b"true") => Some((&input[..4], &input[4..])),
        b'f' if input.starts_with(b"false") => Some((&input[..5], &input[5..])),
        b'n' if input.starts_with(b"null") => Some((&input[..4], &input[4..])),
        b'-' | b'0'..=b'9' => {
            let mut index = 0_usize;
            while index < input.len()
                && matches!(input[index], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
            {
                index += 1;
            }
            (index > 0).then_some((&input[..index], &input[index..]))
        }
        _ => None,
    }
}

fn json_container(input: &[u8], open: u8, close: u8) -> Option<(&[u8], &[u8])> {
    if input.first() != Some(&open) {
        return None;
    }
    let mut depth = 0_usize;
    let mut index = 0_usize;
    while index < input.len() {
        match input[index] {
            byte if byte == open => {
                depth += 1;
                index += 1;
            }
            byte if byte == close => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return Some((&input[..index], &input[index..]));
                }
            }
            b'"' => {
                let (_, rest) = json_string(&input[index..])?;
                index = input.len() - rest.len();
            }
            _ => index += 1,
        }
    }
    None
}

fn trim_start(input: &[u8]) -> &[u8] {
    let index = input
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(input.len());
    &input[index..]
}

fn check_bounded_cap(bytes: &[u8], max: usize, field: &'static str) -> Result<()> {
    if bytes.len() > max {
        Err(CodecError::Limit {
            field,
            actual: bytes.len(),
            max,
        })
    } else {
        Ok(())
    }
}

pub(super) fn unsigned(object: &Map<String, Value>, key: &str, field: &'static str) -> Result<u64> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(CodecError::Invalid(field))
}

pub(super) fn small_unsigned(
    object: &Map<String, Value>,
    key: &str,
    field: &'static str,
) -> Result<u8> {
    unsigned(object, key, field)?
        .try_into()
        .map_err(|_| CodecError::Invalid(field))
}

fn string<'a>(object: &'a Map<String, Value>, key: &str, field: &'static str) -> Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(CodecError::Invalid(field))
}

pub(super) fn decode_fixed_hex<const N: usize>(
    object: &Map<String, Value>,
    key: &str,
    field: &'static str,
) -> Result<[u8; N]> {
    let encoded = string(object, key, field)?.as_bytes();
    if encoded.len() != N * 2 {
        return Err(CodecError::Invalid(field));
    }
    let mut decoded = [0; N];
    for (output, pair) in decoded.iter_mut().zip(encoded.chunks_exact(2)) {
        let nibble = |byte| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(CodecError::Invalid(field)),
        };
        *output = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(decoded)
}

fn fixed_hex<const N: usize, K>(
    object: &Map<String, Value>,
    key: &str,
    field: &'static str,
) -> Result<DomainBytes<N, K>> {
    decode_fixed_hex(object, key, field).map(DomainBytes::new)
}

fn optional_fixed_hex<const N: usize, K>(
    object: &Map<String, Value>,
    key: &str,
    field: &'static str,
) -> Result<Option<DomainBytes<N, K>>> {
    object
        .get(key)
        .map(|_| fixed_hex(object, key, field))
        .transpose()
}

fn segment_generation(object: &Map<String, Value>) -> Result<SegmentGeneration> {
    SegmentGeneration::try_new(small_unsigned(object, "g", "segment generation")?)
}

pub(super) fn timestamp(object: &Map<String, Value>) -> Result<Timestamp> {
    Timestamp::try_new(unsigned(object, "t", "timestamp")?)
}

type DecodedFields<'a> = &'a Map<String, Value>;

pub(super) fn journal_prefix_hash(prefix: &[u8]) -> Result<JournalPrefixHash> {
    if !prefix.ends_with(b"\n") || prefix.contains(&b'\r') {
        return Err(CodecError::Invalid("journal prefix line ending"));
    }
    for row in prefix.split_inclusive(|byte| *byte == b'\n') {
        decode_attempt_record(row)?;
    }
    Ok(JournalPrefixHash::new(Sha256::digest(prefix).into()))
}
