use super::*;
use serde_json::json;

#[test]
fn preserves_schema_property_order() {
    let mut properties = serde_json::Map::new();
    properties.insert("zeta".into(), json!({ "type": "string" }));
    properties.insert("alpha".into(), json!({ "type": "string" }));
    let schema = json!({
        "type": "object",
        "properties": properties
    });
    let specs = parse_form_schema(&schema).unwrap();
    let names: Vec<&str> = specs.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["zeta", "alpha"]);
}

#[test]
fn builds_string_and_required() {
    let schema = json!({
        "type": "object",
        "properties": {
            "email": { "type": "string", "format": "email" },
            "name": { "type": "string" }
        },
        "required": ["email"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    assert_eq!(specs.len(), 2);
    let email = specs.iter().find(|f| f.name == "email").unwrap();
    assert!(email.required);
    assert!(matches!(
        email.kind,
        ElicitFieldKind::String {
            format: Some(ElicitTextFormat::Email),
            ..
        }
    ));
}

#[test]
fn legacy_enum_names_become_labels() {
    let schema = json!({
        "type": "object",
        "properties": {
            "color": {
                "type": "string",
                "enum": ["r", "b"],
                "enumNames": ["Red", "Blue"]
            }
        }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let ElicitFieldKind::SingleSelect { ref options, .. } = specs[0].kind else {
        panic!("expected single-select");
    };
    assert_eq!(options[0].label, "Red");
    assert_eq!(options[0].value, "r");
}

#[test]
fn one_of_titles_become_labels() {
    let schema = json!({
        "type": "object",
        "properties": {
            "env": {
                "oneOf": [
                    { "const": "prod", "title": "Production" },
                    { "const": "dev", "title": "Development" }
                ],
                "default": "dev"
            }
        }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let ElicitFieldKind::SingleSelect {
        ref options,
        default_index,
    } = specs[0].kind
    else {
        panic!("expected single-select");
    };
    assert_eq!(options[0].label, "Production");
    assert_eq!(default_index, Some(1));
}

#[test]
fn multi_select_untitled_parses() {
    let schema = json!({
        "type": "object",
        "properties": {
            "countries": {
                "type": "array",
                "items": { "type": "string", "enum": ["US", "UK", "DE"] },
                "minItems": 1,
                "maxItems": 2,
                "default": ["UK"]
            }
        },
        "required": ["countries"]
    });
    let specs = parse_form_schema(&schema).unwrap();
    let ElicitFieldKind::MultiSelect {
        ref options,
        min_items,
        max_items,
        ref default_indexes,
    } = specs[0].kind
    else {
        panic!("expected multi-select");
    };
    assert_eq!(options.len(), 3);
    assert_eq!((min_items, max_items), (Some(1), Some(2)));
    assert_eq!(default_indexes, &[1]);
}

#[test]
fn multi_select_titled_parses() {
    let schema = json!({
        "type": "object",
        "properties": {
            "features": {
                "type": "array",
                "items": {
                    "anyOf": [
                        { "const": "a", "title": "Alpha" },
                        { "const": "b", "title": "Beta" }
                    ]
                }
            }
        }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let ElicitFieldKind::MultiSelect { ref options, .. } = specs[0].kind else {
        panic!("expected multi-select");
    };
    assert_eq!(options[1].label, "Beta");
}

#[test]
fn array_without_enum_items_is_unsupported_not_fatal() {
    let schema = json!({
        "type": "object",
        "properties": {
            "blobs": { "type": "array", "items": { "type": "object" } },
            "name": { "type": "string" }
        }
    });
    let specs = parse_form_schema(&schema).unwrap();
    assert!(matches!(specs[0].kind, ElicitFieldKind::Unsupported { .. }));
    assert!(matches!(specs[1].kind, ElicitFieldKind::String { .. }));
}

#[test]
fn boolean_default_parses() {
    let schema = json!({
        "type": "object",
        "properties": { "ok": { "type": "boolean", "default": true } }
    });
    let specs = parse_form_schema(&schema).unwrap();
    assert!(matches!(
        specs[0].kind,
        ElicitFieldKind::Boolean { default: true }
    ));
}

#[test]
fn fractional_integer_bounds_tighten_inward() {
    let schema = json!({
        "type": "object",
        "properties": {
            "n": { "type": "integer", "minimum": 0.5, "maximum": 4.5 }
        }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let ElicitFieldKind::Integer {
        minimum, maximum, ..
    } = specs[0].kind
    else {
        panic!("expected integer");
    };
    assert_eq!((minimum, maximum), (Some(1), Some(4)));
}

#[test]
fn non_object_schema_errors() {
    assert!(parse_form_schema(&json!("nope")).is_err());
}

#[test]
fn rejects_too_many_properties() {
    let mut properties = serde_json::Map::new();
    for i in 0..=MAX_ELICIT_FIELDS {
        properties.insert(format!("f{i}"), json!({ "type": "string" }));
    }
    let schema = json!({
        "type": "object",
        "properties": properties
    });
    let err = parse_form_schema(&schema).unwrap_err();
    assert!(
        err.contains(&MAX_ELICIT_FIELDS.to_string()),
        "expected field-cap parse error, got {err:?}"
    );
}

#[test]
fn accepts_max_elicit_fields() {
    let mut properties = serde_json::Map::new();
    for i in 0..MAX_ELICIT_FIELDS {
        properties.insert(format!("f{i}"), json!({ "type": "string" }));
    }
    let schema = json!({
        "type": "object",
        "properties": properties
    });
    let specs = parse_form_schema(&schema).unwrap();
    assert_eq!(specs.len(), MAX_ELICIT_FIELDS);
}

/// Defaults are drafts: a string default longer than the description cap
/// (512) but within the draft cap (4096) must parse, since the user could
/// type the same value by hand.
#[test]
fn string_default_uses_the_draft_cap() {
    let default = "d".repeat(MAX_ELICIT_DESC_CHARS + 1);
    let schema = json!({
        "type": "object",
        "properties": {
            "note": { "type": "string", "default": default }
        }
    });
    let specs = parse_form_schema(&schema).unwrap();
    let ElicitFieldKind::String { ref default, .. } = specs[0].kind else {
        panic!("expected string");
    };
    assert_eq!(
        default.as_deref().map(|d| d.len()),
        Some(MAX_ELICIT_DESC_CHARS + 1)
    );

    let oversized = json!({
        "type": "object",
        "properties": {
            "note": { "type": "string", "default": "d".repeat(MAX_ELICIT_DRAFT_CHARS + 1) }
        }
    });
    assert!(parse_form_schema(&oversized).is_err());
}

#[test]
fn rejects_oversized_title() {
    let schema = json!({
        "type": "object",
        "properties": {
            "email": {
                "type": "string",
                "title": "x".repeat(MAX_ELICIT_TITLE_CHARS + 1)
            }
        }
    });
    assert!(parse_form_schema(&schema).is_err());
}

#[test]
fn rejects_too_many_enum_values() {
    let values: Vec<String> = (0..=MAX_ELICIT_ENUM_VALUES)
        .map(|i| format!("v{i}"))
        .collect();
    let schema = json!({
        "type": "object",
        "properties": {
            "choice": { "type": "string", "enum": values }
        }
    });
    assert!(parse_form_schema(&schema).is_err());
}
