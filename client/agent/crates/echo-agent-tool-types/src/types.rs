use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ext::Extensions;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolDescription {
    /// Tool name (e.g. "web_search", "read_file") that is called by
    /// the model.
    pub name: String,

    /// Optional namespace grouping (e.g. "github", "slack").
    /// None for EchoAgent native tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    /// Display name (e.g. "Web Search") can be shown to the model.
    /// If absent, derive the title from 'name'.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Description of the tool.
    pub description: String,

    /// Raw JSON Schema describing the tool's arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_schema: Option<Value>,

    /// High-level tool kind (stable snake_case, e.g. "read"), set by the tool
    /// server so consumers can group tools by kind. `None` if undeclared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    /// Metadata attached by downstream libraries to support
    /// custom tool behavior. NOT serialized and NOT sent over
    /// the wire.
    ///
    /// Note: 'Extensions' always compares as equal (it carries opaque
    /// runtime data), so 'ToolDescription's derived 'PartialEq' ignores
    /// this field. See 'Extensions' for details.
    #[serde(skip)]
    pub extra: Extensions,
}

impl ToolDescription {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace: None,
            title: None,
            description: description.into(),
            arguments_schema: None,
            kind: None,
            extra: Extensions::new(),
        }
    }

    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    /// Set the high-level tool kind (snake_case string, e.g. "read").
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Attach the raw JSON Schema for this tool's arguments.
    pub fn with_arguments_schema(mut self, schema: impl Into<Value>) -> Self {
        self.arguments_schema = Some(schema.into());
        self
    }

    /// Derive structured arguments from the attached `arguments_schema`.
    ///
    /// **Lossy** — this only extracts flat, top-level properties and a
    /// limited subset of JSON Schema keywords (see
    /// [`parse_arguments_from_schema_lossy`](crate::schema_utils::parse_arguments_from_schema_lossy)
    /// for the full list). Used by UI/render helpers that do not speak full
    /// JSON Schema.
    pub fn to_arguments_lossy(&self) -> Vec<ToolArgument> {
        self.arguments_schema
            .as_ref()
            .map(crate::schema_utils::parse_arguments_from_schema_lossy)
            .unwrap_or_default()
    }

    /// Returns the raw JSON Schema for the tool's arguments if one was
    /// attached via [`Self::with_arguments_schema`].
    pub fn arguments_schema(&self) -> Option<&Value> {
        self.arguments_schema.as_ref()
    }

    /// Return the JSON Schema for this tool's arguments.
    ///
    /// If a raw `arguments_schema` is attached, it is returned as is.
    /// Otherwise returns an empty object schema.
    pub fn to_input_schema(&self) -> serde_json::Value {
        self.arguments_schema.clone().unwrap_or_else(|| {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": [],
            })
        })
    }

    /// Validate structural invariants.
    ///
    /// Checks:
    /// - `name` is non-empty and contains only `[a-zA-Z0-9_-]`.
    /// - `namespace` (if set) follows the same rules.
    ///
    /// Returns all issues found (not just the first).
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = Vec::new();

        validate_identifier("name", &self.name, &mut errors);

        if let Some(ref ns) = self.namespace {
            if ns.is_empty() {
                errors.push(ValidationError {
                    field: "namespace".into(),
                    message: "namespace must not be empty when set".into(),
                });
            } else {
                validate_identifier("namespace", ns, &mut errors);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationErrors(errors))
        }
    }
}

impl fmt::Display for ToolDescription {
    /// Note: this is used for display and debugging purposes only.
    /// This is not a canonical identifier format.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.namespace {
            Some(ns) => write!(f, "{ns}.{}", self.name)?,
            None => write!(f, "{}", self.name)?,
        }
        if !self.description.is_empty() {
            write!(f, " — {}", self.description)?;
        }
        Ok(())
    }
}

/// A single argument for a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolArgument {
    /// Argument name (e.g. "file_path").
    pub name: String,

    /// Human-readable description of the argument.
    pub description: String,

    /// Type of the argument. Accepts both a single JSON Schema type
    /// ("string") and an array of types ("string", "null").
    #[serde(rename = "type", default)]
    pub arg_type: SchemaType,

    /// Schema for Array/Object types. Ignored for primitives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,

    /// Whether the argument is required.
    /// Defaults to true. Omitted from JSON when true.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub required: bool,

    /// Default value for the argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,

    /// Restricted set of valid values for the argument. Empty means no restrictions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<Value>,

    /// Inclusive lower bound (JSON Schema 'minimum').
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<serde_json::Number>,

    /// Inclusive upper bound (JSON Schema 'maximum').
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<serde_json::Number>,

    /// Exclusive lower bound (JSON Schema 'exclusiveMinimum').
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_minimum: Option<serde_json::Number>,

    /// Exclusive upper bound (JSON Schema 'exclusiveMaximum').
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_maximum: Option<serde_json::Number>,
}

impl ToolArgument {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            arg_type: SchemaType::default(),
            schema: None,
            required: true,
            default: None,
            allowed_values: Vec::new(),
            minimum: None,
            maximum: None,
            exclusive_minimum: None,
            exclusive_maximum: None,
        }
    }

    pub fn with_type(mut self, arg_type: impl Into<SchemaType>) -> Self {
        self.arg_type = arg_type.into();
        self
    }

    pub fn with_schema(mut self, schema: impl Into<Value>) -> Self {
        self.schema = Some(schema.into());
        self
    }

    pub fn set_optional(mut self) -> Self {
        self.required = false;
        self
    }

    /// Set a default value for this argument.
    pub fn with_default(mut self, default: impl Into<Value>) -> Self {
        self.default = Some(default.into());
        self
    }

    pub fn with_allowed_values(
        mut self,
        values: impl IntoIterator<Item = impl Into<Value>>,
    ) -> Self {
        self.allowed_values = values.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_minimum(mut self, min: impl Into<serde_json::Number>) -> Self {
        self.minimum = Some(min.into());
        self
    }

    pub fn with_maximum(mut self, max: impl Into<serde_json::Number>) -> Self {
        self.maximum = Some(max.into());
        self
    }

    pub fn with_exclusive_minimum(mut self, min: impl Into<serde_json::Number>) -> Self {
        self.exclusive_minimum = Some(min.into());
        self
    }

    pub fn with_exclusive_maximum(mut self, max: impl Into<serde_json::Number>) -> Self {
        self.exclusive_maximum = Some(max.into());
        self
    }
}

/// Type of a tool argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ArgumentType {
    #[default]
    String,
    Integer,
    Number,
    Boolean,
    Array,
    Object,
    Null,
}

impl ArgumentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Array => "array",
            Self::Object => "object",
            Self::Null => "null",
        }
    }

    pub fn is_primitive(self) -> bool {
        matches!(
            self,
            Self::String | Self::Integer | Self::Number | Self::Boolean | Self::Null
        )
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Integer | Self::Number)
    }

    pub fn is_composite(self) -> bool {
        matches!(self, Self::Array | Self::Object)
    }
}

impl ArgumentType {
    /// Parse from a JSON Schema `"type"` string. Returns `None` for
    /// unrecognised values.
    pub fn from_schema_type(s: &str) -> Option<Self> {
        match s {
            "string" => Some(Self::String),
            "integer" => Some(Self::Integer),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            "array" => Some(Self::Array),
            "object" => Some(Self::Object),
            "null" => Some(Self::Null),
            _ => None,
        }
    }
}

impl fmt::Display for ArgumentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// JSON Schema `"type"` value — either a single type ("string") or
/// an array of types ["string", "null"].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaType {
    /// A single type, e.g. "string".
    Single(ArgumentType),
    /// Multiple types, e.g. ["string", "null"].
    Multiple(Vec<ArgumentType>),
}

impl SchemaType {
    /// Parse a JSON Schema "type" value (string or array) into a
    /// `SchemaType`.
    pub fn from_value(v: &serde_json::Value) -> Self {
        if let Some(s) = v.as_str() {
            return ArgumentType::from_schema_type(s)
                .map(Self::Single)
                .unwrap_or_default();
        }

        if let Some(arr) = v.as_array() {
            let types: Vec<ArgumentType> = arr
                .iter()
                .filter_map(|v| v.as_str())
                .filter_map(ArgumentType::from_schema_type)
                .collect();
            return match types.len() {
                0 => Self::default(),
                1 => Self::Single(types.into_iter().next().unwrap()),
                _ => Self::Multiple(types),
            };
        }

        Self::default()
    }

    /// The "primary" (first non-null) type, used for classification.
    ///
    /// For `Single(t)` this is just `t`.
    /// For `Multiple([String, Null])` this is `String`.
    /// For `Multiple([Null])` or bare `Single(Null)` this is `Null`.
    pub fn primary_type(&self) -> ArgumentType {
        match self {
            Self::Single(t) => *t,
            Self::Multiple(types) => types
                .iter()
                .copied()
                .find(|t| *t != ArgumentType::Null)
                .unwrap_or(ArgumentType::Null),
        }
    }

    /// Whether the type union includes `Null`, i.e. the field accepts
    /// null values alongside its primary type.
    pub fn is_nullable(&self) -> bool {
        match self {
            Self::Single(t) => *t == ArgumentType::Null,
            Self::Multiple(types) => types.contains(&ArgumentType::Null),
        }
    }

    /// Whether the type list contains a specific `ArgumentType`.
    pub fn contains(&self, ty: ArgumentType) -> bool {
        match self {
            Self::Single(t) => *t == ty,
            Self::Multiple(types) => types.contains(&ty),
        }
    }

    /// Returns `true` when every type in the union is primitive
    /// (string, integer, number, boolean, null).
    pub fn is_primitive(&self) -> bool {
        match self {
            Self::Single(t) => t.is_primitive(),
            Self::Multiple(types) => types.iter().all(|t| t.is_primitive()),
        }
    }

    /// Returns `true` when **any** type in the union is composite
    /// (array or object).
    pub fn is_composite(&self) -> bool {
        match self {
            Self::Single(t) => t.is_composite(),
            Self::Multiple(types) => types.iter().any(|t| t.is_composite()),
        }
    }

    /// Returns `true` when **any** type in the union is numeric
    /// (integer or number).
    ///
    /// Numeric bounds (`minimum`, `maximum`, etc.) are only meaningful
    /// when this returns `true`.
    pub fn is_numeric(&self) -> bool {
        match self {
            Self::Single(t) => t.is_numeric(),
            Self::Multiple(types) => types.iter().any(|t| t.is_numeric()),
        }
    }

    /// Return the JSON Schema `"type"` representation.
    pub fn to_schema_value(&self) -> serde_json::Value {
        match self {
            Self::Single(t) => serde_json::Value::String(t.as_str().to_owned()),
            Self::Multiple(types) => serde_json::Value::Array(
                types
                    .iter()
                    .map(|t| serde_json::Value::String(t.as_str().to_owned()))
                    .collect(),
            ),
        }
    }
}

impl Default for SchemaType {
    fn default() -> Self {
        Self::Single(ArgumentType::default())
    }
}

impl From<ArgumentType> for SchemaType {
    fn from(t: ArgumentType) -> Self {
        Self::Single(t)
    }
}

/// Allows `schema_type == ArgumentType::String` without unwrapping.
impl PartialEq<ArgumentType> for SchemaType {
    fn eq(&self, other: &ArgumentType) -> bool {
        matches!(self, Self::Single(t) if t == other)
    }
}

impl fmt::Display for SchemaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Single(t) => fmt::Display::fmt(t, f),
            Self::Multiple(types) => {
                for (i, t) in types.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    fmt::Display::fmt(t, f)?;
                }
                Ok(())
            }
        }
    }
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

// -- Helpers
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

/// Wrapper around multiple ValidationError.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors(pub Vec<ValidationError>);

impl ValidationErrors {
    /// Iterate over the individual errors.
    pub fn iter(&self) -> std::slice::Iter<'_, ValidationError> {
        self.0.iter()
    }

    /// Number of validation errors.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true if there are no errors.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

impl IntoIterator for ValidationErrors {
    type Item = ValidationError;
    type IntoIter = std::vec::IntoIter<ValidationError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a ValidationErrors {
    type Item = &'a ValidationError;
    type IntoIter = std::slice::Iter<'a, ValidationError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

fn validate_identifier(field: &str, value: &str, errors: &mut Vec<ValidationError>) {
    if value.is_empty() {
        errors.push(ValidationError {
            field: field.into(),
            message: format!("{field} must not be empty"),
        });
    } else if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        errors.push(ValidationError {
            field: field.into(),
            message: format!(
                "{field} {value:?} contains invalid characters \
                 (allowed: a-z, A-Z, 0-9, _, -)"
            ),
        });
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_type_serde_roundtrip() {
        let cases = [
            (ArgumentType::String, "\"string\""),
            (ArgumentType::Integer, "\"integer\""),
            (ArgumentType::Number, "\"number\""),
            (ArgumentType::Boolean, "\"boolean\""),
            (ArgumentType::Array, "\"array\""),
            (ArgumentType::Object, "\"object\""),
            (ArgumentType::Null, "\"null\""),
        ];
        for (ty, expected_json) in cases {
            let json = serde_json::to_string(&ty).unwrap();
            assert_eq!(json, expected_json);

            let parsed: ArgumentType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, ty);
        }
    }

    #[test]
    fn argument_type_display() {
        assert_eq!(ArgumentType::String.to_string(), "string");
        assert_eq!(ArgumentType::Integer.to_string(), "integer");
        assert_eq!(ArgumentType::Object.to_string(), "object");
        assert_eq!(ArgumentType::Null.to_string(), "null");
    }

    #[test]
    fn argument_type_rejects_unknown() {
        let result = serde_json::from_str::<ArgumentType>("\"custom_thing\"");
        assert!(result.is_err());
    }

    #[test]
    fn argument_type_classification() {
        assert!(ArgumentType::String.is_primitive());
        assert!(ArgumentType::Integer.is_primitive());
        assert!(ArgumentType::Null.is_primitive());
        assert!(!ArgumentType::Array.is_primitive());
        assert!(!ArgumentType::Object.is_primitive());

        assert!(ArgumentType::Array.is_composite());
        assert!(ArgumentType::Object.is_composite());
        assert!(!ArgumentType::String.is_composite());
        assert!(!ArgumentType::Null.is_composite());
    }

    // -- SchemaType -----------------------------------------------------------

    #[test]
    fn schema_type_single_serde_roundtrip() {
        let st = SchemaType::Single(ArgumentType::String);
        let json = serde_json::to_string(&st).unwrap();
        assert_eq!(json, "\"string\"");
        let parsed: SchemaType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, st);
    }

    #[test]
    fn schema_type_multiple_serde_roundtrip() {
        let st = SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Null]);
        let json = serde_json::to_string(&st).unwrap();
        assert_eq!(json, r#"["string","null"]"#);
        let parsed: SchemaType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, st);
    }

    #[test]
    fn schema_type_from_value_string() {
        let v = serde_json::json!("integer");
        assert_eq!(
            SchemaType::from_value(&v),
            SchemaType::Single(ArgumentType::Integer),
        );
    }

    #[test]
    fn schema_type_from_value_array() {
        let v = serde_json::json!(["string", "null"]);
        assert_eq!(
            SchemaType::from_value(&v),
            SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Null]),
        );
    }

    #[test]
    fn schema_type_from_value_single_element_array_normalised() {
        let v = serde_json::json!(["boolean"]);
        assert_eq!(
            SchemaType::from_value(&v),
            SchemaType::Single(ArgumentType::Boolean),
        );
    }

    #[test]
    fn schema_type_from_value_unknown_falls_back() {
        let v = serde_json::json!("custom_thing");
        assert_eq!(SchemaType::from_value(&v), SchemaType::default());
    }

    #[test]
    fn schema_type_from_value_non_string_non_array_falls_back() {
        let v = serde_json::json!(42);
        assert_eq!(SchemaType::from_value(&v), SchemaType::default());
    }

    #[test]
    fn schema_type_primary_type() {
        assert_eq!(
            SchemaType::Single(ArgumentType::Integer).primary_type(),
            ArgumentType::Integer,
        );
        assert_eq!(
            SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Null]).primary_type(),
            ArgumentType::String,
        );
        // All-null falls back to Null
        assert_eq!(
            SchemaType::Multiple(vec![ArgumentType::Null]).primary_type(),
            ArgumentType::Null,
        );
    }

    #[test]
    fn schema_type_is_nullable() {
        assert!(!SchemaType::Single(ArgumentType::String).is_nullable());
        assert!(SchemaType::Single(ArgumentType::Null).is_nullable());
        assert!(SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Null]).is_nullable());
        assert!(
            !SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Integer]).is_nullable()
        );
    }

    #[test]
    fn schema_type_contains() {
        let st = SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Null]);
        assert!(st.contains(ArgumentType::String));
        assert!(st.contains(ArgumentType::Null));
        assert!(!st.contains(ArgumentType::Integer));
    }

    #[test]
    fn schema_type_primitive_composite_single() {
        assert!(SchemaType::Single(ArgumentType::String).is_primitive());
        assert!(!SchemaType::Single(ArgumentType::String).is_composite());

        assert!(!SchemaType::Single(ArgumentType::Array).is_primitive());
        assert!(SchemaType::Single(ArgumentType::Array).is_composite());

        assert!(SchemaType::Single(ArgumentType::Null).is_primitive());
        assert!(!SchemaType::Single(ArgumentType::Null).is_composite());
    }

    #[test]
    fn schema_type_primitive_composite_multiple() {
        // All primitive → is_primitive=true, is_composite=false
        let nullable_string = SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Null]);
        assert!(nullable_string.is_primitive());
        assert!(!nullable_string.is_composite());

        // Any composite → is_primitive=false, is_composite=true
        let nullable_array = SchemaType::Multiple(vec![ArgumentType::Array, ArgumentType::Null]);
        assert!(!nullable_array.is_primitive());
        assert!(nullable_array.is_composite());

        // Mixed primitive + composite
        let mixed = SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Object]);
        assert!(!mixed.is_primitive());
        assert!(mixed.is_composite());
    }

    #[test]
    fn schema_type_eq_argument_type() {
        assert_eq!(
            SchemaType::Single(ArgumentType::String),
            ArgumentType::String
        );
        assert_ne!(
            SchemaType::Single(ArgumentType::String),
            ArgumentType::Integer
        );
        // Multiple never equals a bare ArgumentType
        assert_ne!(
            SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Null]),
            ArgumentType::String,
        );
    }

    #[test]
    fn schema_type_display() {
        assert_eq!(
            SchemaType::Single(ArgumentType::String).to_string(),
            "string"
        );
        assert_eq!(
            SchemaType::Multiple(vec![ArgumentType::String, ArgumentType::Null]).to_string(),
            "string, null",
        );
    }

    #[test]
    fn schema_type_from_argument_type() {
        let st: SchemaType = ArgumentType::Boolean.into();
        assert_eq!(st, SchemaType::Single(ArgumentType::Boolean));
    }

    #[test]
    fn argument_new_defaults() {
        let arg = ToolArgument::new("query", "Search query");
        assert_eq!(arg.name, "query");
        assert_eq!(arg.arg_type, ArgumentType::String);
        assert!(arg.required);
        assert!(arg.default.is_none());
        assert!(arg.allowed_values.is_empty());
    }

    #[test]
    fn argument_builder_chain() {
        let arg = ToolArgument::new("mode", "Processing mode")
            .with_type(ArgumentType::String)
            .with_allowed_values(["fast", "slow", "auto"])
            .set_optional()
            .with_default(serde_json::json!("auto"));

        assert!(!arg.required);
        assert_eq!(arg.default, Some(serde_json::json!("auto")));
        assert_eq!(arg.allowed_values.len(), 3);
    }

    #[test]
    fn argument_with_default_does_not_change_required() {
        // default and required are orthogonal per JSON Schema
        let arg = ToolArgument::new("x", "test").with_default(serde_json::json!(42));
        assert!(arg.required);
        assert_eq!(arg.default, Some(serde_json::json!(42)));
    }

    #[test]
    fn argument_serde_roundtrip() {
        let arg = ToolArgument::new("count", "Number of items")
            .with_type(ArgumentType::Integer)
            .set_optional()
            .with_default(serde_json::json!(5))
            .with_allowed_values([
                serde_json::json!(1),
                serde_json::json!(5),
                serde_json::json!(10),
            ]);

        let json = serde_json::to_string(&arg).unwrap();
        let parsed: ToolArgument = serde_json::from_str(&json).unwrap();
        assert_eq!(arg, parsed);
    }

    #[test]
    fn argument_serde_required_default_omitted() {
        // Required=true is the default, so it should be omitted from JSON.
        let arg = ToolArgument::new("x", "test");
        let json = serde_json::to_string(&arg).unwrap();
        assert!(
            !json.contains("required"),
            "required=true should be omitted: {json}"
        );

        // Optional should include required=false.
        let arg = ToolArgument::new("x", "test").set_optional();
        let json = serde_json::to_string(&arg).unwrap();
        assert!(
            json.contains("\"required\":false"),
            "required=false should be present: {json}"
        );
    }

    #[test]
    fn argument_deserialize_missing_required_defaults_true() {
        let json = r#"{"name": "x", "description": "test", "type": "string"}"#;
        let arg: ToolArgument = serde_json::from_str(json).unwrap();
        assert!(arg.required);
    }

    #[test]
    fn description_new() {
        let tool = ToolDescription::new("search", "Search things");
        assert_eq!(tool.name, "search");
        assert_eq!(tool.description, "Search things");
        assert!(tool.namespace.is_none());
        assert!(tool.title.is_none());
        assert!(tool.to_arguments_lossy().is_empty());
        assert!(tool.arguments_schema.is_none());
    }

    /// When a raw parameters schema is attached, `to_input_schema` must
    /// return it verbatim — preserving top-level `$defs` so consumers
    /// like xgrammar's structural-tag JSON-schema compiler can resolve
    /// `$ref: "#/$defs/..."` instead of failing with
    /// `Cannot find field $defs in #/$defs/...`.
    #[test]
    fn description_to_input_schema_prefers_raw_with_defs() {
        let raw = serde_json::json!({
            "type": "object",
            "$defs": {
                "PricingItemSpec": {
                    "type": "object",
                    "properties": { "amount": { "type": "number" } }
                }
            },
            "properties": {
                "item": { "$ref": "#/$defs/PricingItemSpec" }
            },
            "required": ["item"]
        });

        let tool =
            ToolDescription::new("price", "Price a thing").with_arguments_schema(raw.clone());

        assert_eq!(tool.to_input_schema(), raw);
    }

    /// Without a raw schema, `to_input_schema` returns an empty object schema.
    #[test]
    fn description_to_input_schema_empty_when_no_raw() {
        let tool = ToolDescription::new("echo", "Echo a string");

        let schema = tool.to_input_schema();
        assert_eq!(schema.get("type"), Some(&serde_json::json!("object")));
        assert_eq!(schema.get("properties"), Some(&serde_json::json!({})),);
    }

    /// Round-trips through serde so the field survives transport
    /// across process or service boundaries.
    #[test]
    fn description_arguments_schema_serde_roundtrip() {
        let raw = serde_json::json!({
            "type": "object",
            "$defs": { "X": { "type": "string", "enum": ["a", "b"] } },
            "properties": { "x": { "$ref": "#/$defs/X" } }
        });
        let tool = ToolDescription::new("t", "d").with_arguments_schema(raw.clone());

        let s = serde_json::to_string(&tool).unwrap();
        let back: ToolDescription = serde_json::from_str(&s).unwrap();
        assert_eq!(back.arguments_schema, Some(raw));
    }

    #[test]
    fn description_builder() {
        let tool = ToolDescription::new("list_repos", "List repositories")
            .with_namespace("github")
            .with_title("List Repositories")
            .with_arguments_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "org": { "type": "string", "description": "Organization name" },
                    "limit": { "type": "integer", "description": "Max results", "default": 30 }
                },
                "required": ["org"]
            }));

        assert_eq!(tool.namespace.as_deref(), Some("github"));
        assert_eq!(tool.to_arguments_lossy().len(), 2);
    }

    #[test]
    fn description_display_name_fallback() {
        let tool = ToolDescription::new("read_file", "Read a file");
        let tool = tool.with_title("Read File");
        // Display uses description, not title
        assert_eq!(tool.to_string(), "read_file — Read a file");
    }

    #[test]
    fn description_display() {
        let tool = ToolDescription::new("search", "Search").with_namespace("github");
        assert_eq!(tool.to_string(), "github.search — Search");

        let plain = ToolDescription::new("stop", "Stop execution");
        assert_eq!(plain.to_string(), "stop — Stop execution");
    }

    #[test]
    fn description_serde_roundtrip() {
        let tool = ToolDescription::new("search", "Search repositories")
            .with_namespace("github")
            .with_title("GitHub Search")
            .with_arguments_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query", "enum": ["code", "issues", "prs"] },
                    "limit": { "type": "integer", "description": "Max results", "default": 25 }
                },
                "required": ["query"]
            }));

        let json = serde_json::to_string_pretty(&tool).unwrap();
        let parsed: ToolDescription = serde_json::from_str(&json).unwrap();
        assert_eq!(tool, parsed);
    }

    #[test]
    fn description_deserialize_minimal() {
        let json = r#"{"name": "stop", "description": "Stop"}"#;
        let tool: ToolDescription = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "stop");
        assert!(tool.to_arguments_lossy().is_empty());
        assert!(tool.extra.is_empty());
    }

    #[test]
    fn validate_ok() {
        let tool = ToolDescription::new("good-tool_1", "A good tool").with_namespace("my-ns");
        assert!(tool.validate().is_ok());
    }

    #[test]
    fn validate_empty_name() {
        let tool = ToolDescription::new("", "desc");
        let errors = tool.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.field == "name"));
    }

    #[test]
    fn validate_invalid_name_chars() {
        let tool = ToolDescription::new("bad name!", "desc");
        let errors = tool.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.field == "name" && e.message.contains("invalid"))
        );
    }

    #[test]
    fn validate_empty_namespace() {
        let mut tool = ToolDescription::new("ok", "desc");
        tool.namespace = Some(String::new());
        let errors = tool.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.field == "namespace"));
    }

    #[test]
    fn validate_collects_all_errors() {
        let mut tool = ToolDescription::new("", "desc");
        tool.namespace = Some(String::new());

        let errors = tool.validate().unwrap_err();
        // empty tool name + empty namespace = 2
        assert!(errors.len() >= 2);
    }
}
