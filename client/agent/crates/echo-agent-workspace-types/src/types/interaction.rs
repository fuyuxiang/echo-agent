//! User-interaction shapes referenced from
//! [`ToolChunk::NeedUserAnswer`](crate::chunks::ToolChunk::NeedUserAnswer)
//! and [`ToolResponse::UserAnswer`](crate::chunks::ToolResponse::UserAnswer).
//!
//! Used by the `ask_user_question` tool flow. The workspace yields
//! a `NeedUserAnswer` chunk carrying a `Vec<UserQuestion>`, the sampler
//! prompts the user and replies with a matching
//! `Vec<UserAnswer>` on the tool's bidi response sender.
//!
//! TODO(workspace): align with the canonical question/answer types in
//! `echo-agent-tools` once the `ask_user_question` tool is extracted
//! into the workspace crate.

use serde::{Deserialize, Serialize};

/// A single question in an `ask_user_question` invocation.
///
/// Wire-only data: pure strings + a bool. The runtime crate is
/// responsible for any UI rendering or option-validation logic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestion {
    /// The complete question text shown to the user.
    pub question: String,
    /// Available options the user can select from.
    #[serde(default)]
    pub options: Vec<UserQuestionOption>,
    /// If true, the user can pick more than one option.
    #[serde(default)]
    pub multi_select: bool,
}

/// One choice within a [`UserQuestion`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestionOption {
    /// Display text for this option (the value the user sees and
    /// selects).
    pub label: String,
    /// Explanation of what this option means or what will happen if
    /// chosen.
    #[serde(default)]
    pub description: String,
    /// Optional preview content (mockup, code snippet, etc.) rendered
    /// when this option is focused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// The user's answer to a single [`UserQuestion`].
///
/// Tagged with `tag = "type", content = "data"` (adjacent tagging) to
/// match every other wire enum in the crate. See `crate::lib`
/// doc-comment "# Wire format" for the rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum UserAnswer {
    /// The user picked one of the listed options (by label).
    Selected(String),
    /// The user picked the "Other" choice with custom free-form text.
    Other(String),
    /// The user picked multiple options (only valid when the
    /// corresponding [`UserQuestion::multi_select`] is `true`).
    Multiple(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_answer_samples() -> Vec<UserAnswer> {
        vec![
            UserAnswer::Selected("Option A".into()),
            UserAnswer::Other("freeform answer".into()),
            UserAnswer::Multiple(vec!["A".into(), "C".into()]),
        ]
    }

    #[test]
    fn user_question_round_trips_through_json() {
        let q = UserQuestion {
            question: "Pick a color?".into(),
            options: vec![
                UserQuestionOption {
                    label: "Red".into(),
                    description: "warm".into(),
                    preview: Some("```css\ncolor: red;\n```".into()),
                },
                UserQuestionOption {
                    label: "Blue".into(),
                    description: "cool".into(),
                    preview: None,
                },
            ],
            multi_select: false,
        };
        let json = serde_json::to_string(&q).unwrap();
        let back: UserQuestion = serde_json::from_str(&json).unwrap();
        assert_eq!(q, back);
    }

    #[test]
    fn user_question_uses_snake_case_field_names() {
        let q = UserQuestion {
            question: "?".into(),
            options: vec![],
            multi_select: true,
        };
        let json = serde_json::to_string(&q).unwrap();
        assert!(json.contains("\"multi_select\""), "got {json}");
        assert!(!json.contains("\"multiSelect\""), "got {json}");
    }

    #[test]
    fn user_question_option_omits_preview_when_none() {
        let opt = UserQuestionOption {
            label: "x".into(),
            description: "y".into(),
            preview: None,
        };
        let json = serde_json::to_string(&opt).unwrap();
        assert!(
            !json.contains("preview"),
            "preview should be skipped: {json}"
        );
    }

    #[test]
    fn user_answer_round_trips_for_every_variant() {
        for a in user_answer_samples() {
            let json = serde_json::to_string(&a).unwrap();
            let back: UserAnswer = serde_json::from_str(&json).unwrap();
            assert_eq!(a, back);
        }
    }

    #[test]
    fn user_answer_uses_adjacent_type_data_tag() {
        let a = UserAnswer::Selected("opt".into());
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, r#"{"type":"selected","data":"opt"}"#);

        let m = UserAnswer::Multiple(vec!["a".into(), "b".into()]);
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, r#"{"type":"multiple","data":["a","b"]}"#);
    }
}
