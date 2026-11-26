use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::{
    codec::*,
    codec_tests,
    decoder::{DecodedAttemptRecordV1, decode_attempt_record},
};

fn encoded(record: &RecordV1) -> Vec<u8> {
    EncodedRecord::try_new(record).unwrap().as_bytes().to_vec()
}

fn decode_core(bytes: &[u8]) -> Result<RecordV1> {
    match decode_attempt_record(bytes)? {
        DecodedAttemptRecordV1::Core(record) => Ok(record),
        DecodedAttemptRecordV1::Rewind(_) => Err(CodecError::Invalid("record event")),
    }
}

fn replace_once(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    let start = bytes
        .windows(old.len())
        .position(|window| window == old)
        .unwrap();
    let mut changed = Vec::with_capacity(bytes.len() - old.len() + new.len());
    changed.extend_from_slice(&bytes[..start]);
    changed.extend_from_slice(new);
    changed.extend_from_slice(&bytes[start + old.len()..]);
    changed
}

fn with_field_byte(bytes: &[u8], key: &str, value: u8) -> Vec<u8> {
    let needle = format!("\"{key}\":\"");
    let start = bytes
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())
        .unwrap()
        + needle.len();
    let mut changed = bytes.to_vec();
    changed[start] = value;
    changed
}

#[test]
fn every_event_roundtrips_exact_canonical_bytes() {
    for record in codec_tests::records(codec_tests::text(b"generic lineage")) {
        let bytes = encoded(&record);
        let decoded = decode_core(&bytes).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(encoded(&decoded), bytes);
        assert_eq!(
            decode_attempt_record(&bytes).unwrap(),
            DecodedAttemptRecordV1::Core(record)
        );
    }
}

#[test]
fn independent_dispatch_key_order_and_optional_shape_goldens() {
    let rows = [
        b"{\"v\":1,\"e\":0,\"a\":\"01010101010101010101010101010101\",\"r\":\"02020202020202020202020202020202\",\"g\":1,\"k\":0,\"h\":\"0303030303030303030303030303030303030303030303030303030303030303\",\"t\":0}\n".as_slice(),
        b"{\"v\":1,\"e\":2,\"g\":2,\"k\":2,\"m\":\"04040404040404040404040404040404\",\"s\":\"05050505050505050505050505050505\",\"r\":0,\"a\":0,\"c\":\"bGF0ZXI\",\"t\":0}\n",
        b"{\"v\":1,\"e\":6,\"g\":1,\"o\":1,\"c\":\"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b\",\"t\":0}\n",
    ];
    for row in rows {
        assert_eq!(encoded(&decode_core(row).unwrap()), row);
    }
}

#[test]
fn global_event_and_content_caps_precede_allocating_json_parse() {
    let global_over = vec![b' '; MAX_ENCODED_RECORD_BYTES + 1];
    assert!(matches!(
        decode_core(&global_over),
        Err(CodecError::Limit {
            field: "encoded record",
            max: MAX_ENCODED_RECORD_BYTES,
            ..
        })
    ));
    let event_over = format!(
        "{{\"v\":1,\"e\":4,\"g\":1,\"t\":0,\"z\":\"{}\"}}\n",
        "a".repeat(64)
    );
    assert!(matches!(
        decode_core(event_over.as_bytes()),
        Err(CodecError::Limit {
            field: "encoded record",
            max: 64,
            ..
        })
    ));

    for raw_len in [
        MAX_MESSAGE_RAW_BYTES - 2,
        MAX_MESSAGE_RAW_BYTES - 1,
        MAX_MESSAGE_RAW_BYTES,
    ] {
        let boundary = codec_tests::records(codec_tests::text(&vec![b'a'; raw_len])).remove(2);
        let bytes = encoded(&boundary);
        assert_eq!(decode_core(&bytes).unwrap(), boundary);
    }

    let content = URL_SAFE_NO_PAD.encode(vec![b'a'; MAX_MESSAGE_RAW_BYTES + 1]);
    let over = format!(
        "{{\"v\":1,\"e\":2,\"g\":1,\"k\":1,\"m\":\"{}\",\"s\":\"{}\",\"x\":\"{}\",\"r\":0,\"a\":0,\"c\":\"{content}\",\"t\":0}}\n",
        "01".repeat(16),
        "02".repeat(16),
        "03".repeat(16)
    );
    assert!(matches!(
        decode_core(over.as_bytes()),
        Err(CodecError::Limit {
            field: "encoded agent text",
            max: 43_691,
            ..
        })
    ));
}

#[test]
fn strict_line_dispatch_key_and_numeric_alternates_reject() {
    let valid = encoded(&codec_tests::records(codec_tests::text(b""))[4]);
    let invalid = [
        ("empty", b"".as_slice()),
        ("missing LF", valid.strip_suffix(b"\n").unwrap()),
        ("extra LF", b"{\"v\":1,\"e\":4,\"g\":33,\"t\":0}\n\n"),
        ("CRLF", b"{\"v\":1,\"e\":4,\"g\":33,\"t\":0}\r\n"),
        ("interior LF", b"{\"v\":1,\n\"e\":4,\"g\":33,\"t\":0}\n"),
        ("unknown version", b"{\"v\":2,\"e\":4,\"g\":33,\"t\":0}\n"),
        ("unknown event", b"{\"v\":1,\"e\":11,\"g\":33,\"t\":0}\n"),
        ("lexical version", b"{\"v\":1.0,\"e\":4,\"g\":33,\"t\":0}\n"),
        ("lexical event", b"{\"v\":1,\"e\":04,\"g\":33,\"t\":0}\n"),
        (
            "reordered dispatch",
            b"{\"e\":4,\"v\":1,\"g\":33,\"t\":0}\n",
        ),
        (
            "leading whitespace",
            b" {\"v\":1,\"e\":4,\"g\":33,\"t\":0}\n",
        ),
        (
            "duplicate",
            b"{\"v\":1,\"e\":4,\"g\":33,\"g\":33,\"t\":0}\n",
        ),
        (
            "unknown key",
            b"{\"v\":1,\"e\":4,\"g\":33,\"z\":0,\"t\":0}\n",
        ),
        ("missing key", b"{\"v\":1,\"e\":4,\"t\":0}\n"),
        ("reordered keys", b"{\"v\":1,\"e\":4,\"t\":0,\"g\":33}\n"),
        (
            "numeric whitespace",
            b"{\"v\":1,\"e\":4,\"g\": 33,\"t\":0}\n",
        ),
        ("negative integer", b"{\"v\":1,\"e\":4,\"g\":-1,\"t\":0}\n"),
        ("float integer", b"{\"v\":1,\"e\":4,\"g\":33.0,\"t\":0}\n"),
        (
            "string integer",
            b"{\"v\":1,\"e\":4,\"g\":\"33\",\"t\":0}\n",
        ),
        ("escaped key", b"{\"\\u0076\":1,\"e\":4,\"g\":33,\"t\":0}\n"),
        ("extra whitespace", b"{\"v\":1, \"e\":4,\"g\":33,\"t\":0}\n"),
    ];
    for (case, bytes) in invalid {
        assert!(decode_core(bytes).is_err(), "{case}");
    }
}

#[test]
fn every_event_rejects_an_invalid_ordinal_or_scalar() {
    let records = codec_tests::records(codec_tests::text(b""));
    let invalid = [
        replace_once(&encoded(&records[0]), b"\"k\":1", b"\"k\":2"),
        replace_once(&encoded(&records[1]), b"\"s\":33", b"\"s\":34"),
        replace_once(&encoded(&records[2]), b"\"r\":0", b"\"r\":1"),
        replace_once(&encoded(&records[3]), b"\"g\":33", b"\"g\":0"),
        replace_once(&encoded(&records[4]), b"\"g\":33", b"\"g\":34"),
        replace_once(
            &encoded(&records[5]),
            b"\"t\":9999999999999",
            b"\"t\":10000000000000",
        ),
        replace_once(&encoded(&records[6]), b"\"o\":0", b"\"o\":3"),
        replace_once(&encoded(&records[7]), b"\"r\":2", b"\"r\":1"),
        replace_once(
            &encoded(&records[8]),
            b"18446744073709551615",
            b"18446744073709551616",
        ),
        replace_once(&encoded(&records[9]), b"\"o\":3", b"\"o\":4"),
        replace_once(&encoded(&records[10]), b"\"w\":33", b"\"w\":34"),
    ];
    for (event, bytes) in invalid.iter().enumerate() {
        assert!(decode_core(bytes).is_err(), "event {event}");
    }
}

#[test]
fn every_identifier_and_hash_domain_rejects_non_lowercase_hex() {
    let records = codec_tests::records(codec_tests::text(b""));
    for (event, key) in [
        (0, "a"),
        (0, "r"),
        (0, "h"),
        (2, "m"),
        (2, "s"),
        (2, "x"),
        (3, "p"),
        (3, "h"),
        (5, "p"),
        (5, "c"),
        (6, "c"),
        (6, "r"),
    ] {
        let bytes = with_field_byte(&encoded(&records[event]), key, b'A');
        assert!(decode_core(&bytes).is_err(), "event {event} key {key}");
    }
}

#[test]
fn content_lineage_width_and_closed_products_reject() {
    let records = codec_tests::records(codec_tests::text(b""));
    let accepted = encoded(&records[2]);
    let invalid = [
        with_field_byte(&encoded(&records[0]), "a", b'g'),
        replace_once(
            &encoded(&records[0]),
            b"01010101010101010101010101010101",
            b"010101010101010101010101010101",
        ),
        replace_once(
            &accepted,
            b",\"x\":\"06060606060606060606060606060606\"",
            b"",
        ),
        replace_once(&accepted, b"\"g\":1,\"k\":1", b"\"g\":2,\"k\":2"),
        replace_once(&accepted, b"\"c\":\"\"", b"\"c\":\"YQ==\""),
        replace_once(&accepted, b"\"c\":\"\"", b"\"c\":\"AB\""),
        replace_once(&accepted, b"\"c\":\"\"", b"\"c\":\"_w\""),
        replace_once(&accepted, b"\"c\":\"\"", b"\"c\":\"\\u0059Q\""),
        replace_once(&encoded(&records[3]), b"\"g\":33,\"b\"", b"\"g\":1,\"b\""),
        replace_once(&encoded(&records[6]), b"\"o\":0", b"\"o\":1"),
        replace_once(
            &encoded(&records[10]),
            b"\"o\":3,\"r\":3",
            b"\"o\":0,\"r\":1",
        ),
    ];
    for (case, bytes) in invalid.iter().enumerate() {
        assert!(decode_core(bytes).is_err(), "case {case}");
    }
}

#[test]
fn content_bound_rejects_whitespace_and_escaped_duplicate_keys() {
    let accepted = encoded(&codec_tests::records(codec_tests::text(b""))[2]);
    let oversize = "a".repeat(43_691 + 1);
    for injection in [
        format!("\"c\" : \"{oversize}\",\"r\":0,\"a\":0,\"c\":\"\""),
        format!("\"c\"\t:\t\"{oversize}\",\"r\":0,\"a\":0,\"c\":\"\""),
        format!("\"\\u0063\":\"{oversize}\",\"r\":0,\"a\":0,\"c\":\"\""),
        format!("\"\\u0063\" : \"{oversize}\",\"r\":0,\"a\":0,\"c\":\"\""),
    ] {
        let bytes = replace_once(
            &accepted,
            b"\"r\":0,\"a\":0,\"c\":\"\"",
            injection.as_bytes(),
        );
        assert!(
            bytes.len() <= MAX_ENCODED_RECORD_BYTES,
            "fixture must stay under the event/global row cap"
        );
        assert!(
            matches!(
                decode_core(&bytes),
                Err(CodecError::Limit {
                    field: "encoded agent text",
                    max: 43_691,
                    ..
                })
            ),
            "expected content limit, got {:?}",
            decode_core(&bytes)
        );
    }

    // Exact-byte duplicate remains rejected.
    let exact_dup = replace_once(
        &accepted,
        b"\"r\":0,\"a\":0,\"c\":\"\"",
        b"\"c\":\"\",\"r\":0,\"a\":0,\"c\":\"\"",
    );
    assert!(decode_core(&exact_dup).is_err());
}
