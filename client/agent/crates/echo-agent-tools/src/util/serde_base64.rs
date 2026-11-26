//! Encodes a byte payload as a base64 string instead of a JSON integer array
//! (~4x smaller), for bash output streamed over the hub WebSocket via
//! `BashNotificationBase.output`.
//!
//! The deserializer accepts both the base64 string and the legacy integer-array
//! form, so a new consumer can read an old producer; the serializer always emits
//! base64. The CHANGELOG (2026-05-29) covers the consumer-before-producer deploy
//! ordering this implies. Requires a self-describing format (JSON): the dual-form
//! detection and the `#[serde(flatten)]` on the notification structs both force
//! `deserialize_any`.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::Serializer;

/// Serialize a byte slice as a base64 string.
pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&STANDARD.encode(bytes))
}

/// Deserialize `Vec<u8>` from either a base64 string (new form) or an integer
/// array (legacy form).
pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(BytesVisitor)
}

/// Accepts both a base64 string and a legacy integer array.
struct BytesVisitor;

impl<'de> Visitor<'de> for BytesVisitor {
    type Value = Vec<u8>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a base64 string or an array of bytes")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        STANDARD.decode(v).map_err(de::Error::custom)
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(v.to_vec())
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(v)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut bytes = Vec::new();
        while let Some(b) = seq.next_element::<u8>()? {
            bytes.push(b);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Wrapper {
        #[serde(with = "super")]
        output: Vec<u8>,
    }

    #[test]
    fn vec_round_trips_binary_bytes() {
        let original = Wrapper {
            output: vec![0x00, 0xff, 0xfe, 0x80, 0x01, b'h', b'i'],
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn vec_serializes_to_string_not_array() {
        let w = Wrapper {
            output: b"hello".to_vec(),
        };
        let value = serde_json::to_value(&w).unwrap();
        assert!(
            value["output"].is_string(),
            "expected base64 string, got {value:?}"
        );
        assert_eq!(value["output"], json!("aGVsbG8="));
    }

    #[test]
    fn vec_reads_legacy_integer_array() {
        let legacy = json!({ "output": [104, 101, 108, 108, 111] });
        let w: Wrapper = serde_json::from_value(legacy).unwrap();
        assert_eq!(w.output, b"hello".to_vec());
    }

    #[test]
    fn vec_rejects_malformed_base64() {
        for bad in [json!("!!!"), json!("abc")] {
            let r: Result<Wrapper, _> = serde_json::from_value(json!({ "output": bad }));
            assert!(r.is_err(), "malformed base64 {bad:?} must error");
        }
    }

    #[test]
    fn vec_rejects_unexpected_type() {
        let r: Result<Wrapper, _> = serde_json::from_value(json!({ "output": 5 }));
        assert!(r.is_err(), "numeric scalar must error");
    }

    #[test]
    fn vec_empty_round_trips() {
        let w = Wrapper { output: vec![] };
        let value = serde_json::to_value(&w).unwrap();
        assert_eq!(value["output"], json!(""));
        let back: Wrapper = serde_json::from_value(value).unwrap();
        assert_eq!(back.output, Vec::<u8>::new());
    }

    #[test]
    fn vec_round_trips_large_binary_buffer() {
        // Deterministic pseudo-random bytes including 0x00 and 0xff.
        let mut data = vec![0u8; 20_000];
        let mut state: u32 = 0x1234_5678;
        for (i, b) in data.iter_mut().enumerate() {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *b = (state >> 16) as u8;
            if i % 257 == 0 {
                *b = 0x00;
            } else if i % 263 == 0 {
                *b = 0xff;
            }
        }
        assert!(data.contains(&0x00) && data.contains(&0xff));

        // A bare `Vec<u8>` serializes as the legacy integer array — the baseline
        // the base64 form must beat. Measure it before moving `data`.
        let int_array_len = serde_json::to_string(&data).unwrap().len();

        let w = Wrapper { output: data };
        let base64_json = serde_json::to_string(&w).unwrap();
        let back: Wrapper = serde_json::from_str(&base64_json).unwrap();
        assert_eq!(back.output, w.output);
        assert!(
            base64_json.len() < int_array_len,
            "base64 ({}) should be smaller than int-array ({int_array_len})",
            base64_json.len(),
        );
    }
}
