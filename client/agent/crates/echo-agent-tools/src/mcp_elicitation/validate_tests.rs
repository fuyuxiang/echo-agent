use super::super::schema::{ElicitFieldKind, ElicitFieldSpec, ElicitTextFormat, parse_form_schema};
use super::*;
use serde_json::json;

fn draft_values<'a>(specs: &[ElicitFieldSpec], drafts: &'a [&'a str]) -> Vec<ElicitFieldValue<'a>> {
    specs
        .iter()
        .enumerate()
        .map(|(i, _)| ElicitFieldValue::Draft(drafts.get(i).copied().unwrap_or("")))
        .collect()
}

fn string_spec(format: Option<ElicitTextFormat>) -> ElicitFieldSpec {
    ElicitFieldSpec {
        name: "s".into(),
        title: "s".into(),
        description: None,
        required: true,
        kind: ElicitFieldKind::String {
            format,
            min_length: None,
            max_length: None,
            default: None,
        },
    }
}

#[test]
fn rejects_missing_required() {
    let schema = json!({
        "type": "object",
        "properties": { "email": { "type": "string" } },
        "required": ["email"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    let err = validate_form(&specs, &draft_values(&specs, &[""])).unwrap_err();
    assert_eq!(err[0].field, "email");
}

#[test]
fn string_values_are_submitted_verbatim() {
    let schema = json!({
        "type": "object",
        "properties": {
            "note": { "type": "string", "minLength": 6 }
        },
        "required": ["note"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    // Whitespace is part of the value: it counts toward minLength and is
    // preserved in the accepted content, exactly as the user reviewed it.
    let content = validate_form(&specs, &draft_values(&specs, &["  ab  "])).unwrap();
    assert_eq!(content["note"], "  ab  ");
    assert!(
        validate_form(&specs, &draft_values(&specs, &["  ab"])).is_err(),
        "4 chars including spaces is below minLength 6"
    );
}

#[test]
fn whitespace_only_string_is_a_value() {
    let schema = json!({
        "type": "object",
        "properties": { "sep": { "type": "string" } }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(&specs, &draft_values(&specs, &["   "])).unwrap();
    assert_eq!(content["sep"], "   ");
}

#[test]
fn accepts_valid_email() {
    let schema = json!({
        "type": "object",
        "properties": { "email": { "type": "string", "format": "email" } },
        "required": ["email"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(&specs, &draft_values(&specs, &["user@example.com"])).unwrap();
    assert_eq!(content["email"], "user@example.com");
}

#[test]
fn rejects_bad_emails() {
    let spec = string_spec(Some(ElicitTextFormat::Email));
    for bad in [
        "not-an-email",
        "a b@example.com",
        "user@",
        "user@nodot",
        "user@@example.com",
        "user@-bad.com",
        "user@bad-.com",
        " user@example.com",
    ] {
        assert!(
            validate_field(&spec, &ElicitFieldValue::Draft(bad)).is_err(),
            "{bad:?} should be rejected"
        );
    }
    for good in ["user@example.com", "a.b+c@sub.example.co"] {
        assert!(
            validate_field(&spec, &ElicitFieldValue::Draft(good)).is_ok(),
            "{good} should be accepted"
        );
    }
}

#[test]
fn uri_format_accepts_non_http_uris() {
    let spec = string_spec(Some(ElicitTextFormat::Uri));
    for good in [
        "https://example.com/a?b=1",
        "urn:isbn:0451450523",
        "mailto:user@example.com",
        "ftp://files.example.com/pub",
    ] {
        assert!(
            validate_field(&spec, &ElicitFieldValue::Draft(good)).is_ok(),
            "{good} should be accepted"
        );
    }
    for bad in ["not a uri", "/relative/only", "http//missing-colon"] {
        assert!(
            validate_field(&spec, &ElicitFieldValue::Draft(bad)).is_err(),
            "{bad} should be rejected"
        );
    }
}

#[test]
fn date_format_is_calendar_aware() {
    let spec = string_spec(Some(ElicitTextFormat::Date));
    for good in ["2024-02-29", "2026-12-31"] {
        assert!(
            validate_field(&spec, &ElicitFieldValue::Draft(good)).is_ok(),
            "{good} should be accepted"
        );
    }
    for bad in [
        "2023-02-29",
        "2026-13-01",
        "2026-00-10",
        "2026-1-1",
        "garbage",
    ] {
        assert!(
            validate_field(&spec, &ElicitFieldValue::Draft(bad)).is_err(),
            "{bad} should be rejected"
        );
    }
}

#[test]
fn date_time_format_is_rfc3339() {
    let spec = string_spec(Some(ElicitTextFormat::DateTime));
    for good in ["2026-08-19T10:00:00Z", "2026-08-19T10:00:00.123+02:00"] {
        assert!(
            validate_field(&spec, &ElicitFieldValue::Draft(good)).is_ok(),
            "{good} should be accepted"
        );
    }
    for bad in ["2026-08-19", "2026-08-19 10:00:00", "2026-08-19T25:00:00Z"] {
        assert!(
            validate_field(&spec, &ElicitFieldValue::Draft(bad)).is_err(),
            "{bad} should be rejected"
        );
    }
}

#[test]
fn unknown_format_is_not_validated() {
    let schema = json!({
        "type": "object",
        "properties": {
            "ref": { "type": "string", "format": "uri-reference" }
        },
        "required": ["ref"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(&specs, &draft_values(&specs, &["/relative/path"])).unwrap();
    assert_eq!(content["ref"], "/relative/path");
}

#[test]
fn number_min_max() {
    let schema = json!({
        "type": "object",
        "properties": {
            "ratio": { "type": "number", "minimum": 0.5, "maximum": 2.5 }
        },
        "required": ["ratio"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    assert!(validate_form(&specs, &draft_values(&specs, &["1.25"])).is_ok());
    assert!(validate_form(&specs, &draft_values(&specs, &["0.1"])).is_err());
    assert!(validate_form(&specs, &draft_values(&specs, &["nan"])).is_err());
}

#[test]
fn integer_is_parsed_losslessly() {
    let schema = json!({
        "type": "object",
        "properties": {
            "age": { "type": "integer", "minimum": 0, "maximum": 120 }
        },
        "required": ["age"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(&specs, &draft_values(&specs, &["30"])).unwrap();
    assert_eq!(content["age"], 30);
    for bad in ["30.5", "200", "1e20", "9223372036854775808"] {
        assert!(
            validate_form(&specs, &draft_values(&specs, &[bad])).is_err(),
            "{bad} should be rejected"
        );
    }
}

#[test]
fn large_integer_keeps_every_digit() {
    let schema = json!({
        "type": "object",
        "properties": { "id": { "type": "integer" } },
        "required": ["id"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    // Above 2^53: an f64 round-trip would change the value.
    let content = validate_form(&specs, &draft_values(&specs, &["9007199254740993"])).unwrap();
    assert_eq!(content["id"], 9007199254740993_i64);
}

#[test]
fn fractional_integer_bounds_apply() {
    let schema = json!({
        "type": "object",
        "properties": {
            "n": { "type": "integer", "minimum": 0.5, "maximum": 4.5 }
        },
        "required": ["n"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    assert!(validate_form(&specs, &draft_values(&specs, &["0"])).is_err());
    assert!(validate_form(&specs, &draft_values(&specs, &["1"])).is_ok());
    assert!(validate_form(&specs, &draft_values(&specs, &["4"])).is_ok());
    assert!(validate_form(&specs, &draft_values(&specs, &["5"])).is_err());
}

#[test]
fn single_select_field() {
    let schema = json!({
        "type": "object",
        "properties": {
            "color": { "type": "string", "enum": ["red", "blue"] }
        },
        "required": ["color"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    assert!(matches!(
        specs[0].kind,
        ElicitFieldKind::SingleSelect { .. }
    ));
    let content = validate_form(&specs, &[ElicitFieldValue::Choice(Some(1))]).unwrap();
    assert_eq!(content["color"], "blue");
    assert!(validate_form(&specs, &[ElicitFieldValue::Choice(None)]).is_err());
}

#[test]
fn multi_select_validates_items() {
    let schema = json!({
        "type": "object",
        "properties": {
            "countries": {
                "type": "array",
                "items": { "type": "string", "enum": ["US", "UK", "DE"] },
                "minItems": 1,
                "maxItems": 2
            }
        },
        "required": ["countries"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(&specs, &[ElicitFieldValue::MultiChoice(&[0, 2])]).unwrap();
    assert_eq!(content["countries"], json!(["US", "DE"]));
    // minItems enforced.
    assert!(validate_form(&specs, &[ElicitFieldValue::MultiChoice(&[])]).is_err());
    // maxItems enforced.
    assert!(validate_form(&specs, &[ElicitFieldValue::MultiChoice(&[0, 1, 2])]).is_err());
}

#[test]
fn optional_empty_multi_select_is_omitted() {
    let schema = json!({
        "type": "object",
        "properties": {
            "features": {
                "type": "array",
                "items": { "type": "string", "enum": ["a", "b"] }
            }
        }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(&specs, &[ElicitFieldValue::MultiChoice(&[])]).unwrap();
    assert!(content.is_empty());
}

/// `required` only demands presence and `minItems` defaults to 0, so an
/// empty required multi-select submits `[]` (an explicit `minItems: 1`
/// is the schema's way to demand a selection).
#[test]
fn required_multi_select_submits_empty_array_without_min_items() {
    let schema = json!({
        "type": "object",
        "properties": {
            "tags": {
                "type": "array",
                "items": { "type": "string", "enum": ["x", "y"] }
            }
        },
        "required": ["tags"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(&specs, &[ElicitFieldValue::MultiChoice(&[])]).unwrap();
    assert_eq!(content["tags"], json!([]));
}

#[test]
fn required_unsupported_field_errors() {
    let schema = json!({
        "type": "object",
        "properties": { "blob": { "type": "object" } },
        "required": ["blob"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    let err = validate_form(&specs, &[ElicitFieldValue::Draft("")]).unwrap_err();
    assert_eq!(err[0].message, "unsupported field type");
}

#[test]
fn optional_unsupported_field_is_omitted() {
    let schema = json!({
        "type": "object",
        "properties": {
            "blobs": { "type": "array", "items": { "type": "object" } },
            "name": { "type": "string" }
        }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(
        &specs,
        &[ElicitFieldValue::Draft(""), ElicitFieldValue::Draft("n")],
    )
    .unwrap();
    assert_eq!(content["name"], "n");
    assert!(!content.contains_key("blobs"));
}

#[test]
fn boolean_field() {
    let schema = json!({
        "type": "object",
        "properties": { "ok": { "type": "boolean", "default": true } }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let content = validate_form(&specs, &[ElicitFieldValue::Bool(true)]).unwrap();
    assert_eq!(content["ok"], true);
}
