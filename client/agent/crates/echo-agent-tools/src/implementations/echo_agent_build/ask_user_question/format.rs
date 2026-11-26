//! Formatting functions for AskUserQuestion tool results.
//!
//! Each function produces the **exact** model-visible string for one of the
//! four user-action paths.
//!
//! The tests below pin the exact output strings and serve as the
//! source-of-truth specification.

use std::collections::HashMap;

use indexmap::IndexMap;

use super::Question;
use super::types::QuestionAnnotation;

// ── Path D: Cancel ──────────────────────────────────────────────────────

/// Tool result text when the user cancels / dismisses the question UI.
///
/// Cancel is a normal user decision, not a tool failure, so this is a
/// purpose-built message rather than a generic permission-denial string.
pub const CANCEL_TEXT: &str = "User declined to answer the questions. Continue with the task using your best judgment, or ask different questions.";

/// Tool result text for unanswered questionnaires in non-interactive sessions
/// (headless `-p`, SDK): there is no user, so "user declined" would be a lie.
pub const NO_OPERATOR_TEXT: &str = "No user is available to answer questions in this non-interactive session. Continue with your best judgment; do not wait for clarification.";

/// Single source for the unanswered (cancel / timeout) tool result text, so
/// the two paths cannot drift between interactive and non-interactive wording.
pub fn unanswered_text(non_interactive: bool) -> &'static str {
    if non_interactive {
        NO_OPERATOR_TEXT
    } else {
        CANCEL_TEXT
    }
}

// ── Path A: Accepted ────────────────────────────────────────────────────

/// Format the tool result for Path A (user accepted and submitted answers).
///
/// Produces the accepted-answers tool result:
///
/// ```text
/// User has answered your questions: "<q>"="<label>" ..., "<q>"="<label>" .... You can now continue with the user's answers in mind.
/// ```
///
/// Rules:
/// - Only answered questions appear (unanswered are omitted by the caller).
/// - Multi-select: each selected label is its own `Vec` element on the
///   wire; this function joins them with `, ` at format time.
/// - Freeform-only: a single-element vec containing `"Other"`, free text
///   in `annotations[q].notes`.
/// - Preview is appended only when present in annotations.
/// - Notes are appended only when present in annotations.
/// - Questions/labels are interpolated raw (no escaping).
pub fn format_accepted_tool_result(
    answers: &IndexMap<String, Vec<String>>,
    annotations: &Option<HashMap<String, QuestionAnnotation>>,
) -> String {
    let entries: Vec<String> = answers
        .iter()
        .map(|(question_text, selected_labels)| {
            let selected_label = selected_labels.join(", ");
            let mut parts = vec![format!("\"{}\"=\"{}\"", question_text, selected_label)];

            if let Some(anns) = annotations
                && let Some(ann) = anns.get(question_text)
            {
                if let Some(ref preview) = ann.preview {
                    parts.push(format!("selected preview:\n{}", preview));
                }
                if let Some(ref notes) = ann.notes {
                    parts.push(format!("user notes: {}", notes));
                }
            }

            parts.join(" ")
        })
        .collect();

    format!(
        "User has answered your questions: {}. You can now continue with the user's answers in mind.",
        entries.join(", ")
    )
}

// ── Alternate id-keyed tool-result formatting ────

/// Format the tool result in the alternate id-keyed shape (Path A).
///
/// Answers are keyed by **id**, one question per line, with no trailing
/// sentence:
///
/// ```text
/// User questions responses:
/// Question <qid>: Selected option(s) <oid>(, <oid>)*
/// Question <qid>: Selected option(s) <oid>(, <oid>)*
/// ```
///
/// Examples:
///
/// - Single question, single-select:
///   `User questions responses:\nQuestion demo_pick: Selected option(s) a`
/// - Three questions, last with `allow_multiple: true` (one selection):
///   `User questions responses:\nQuestion q1: Selected option(s) tea\nQuestion q2: Selected option(s) code\nQuestion q3: Selected option(s) tests`
///
/// Multi-select labels arrive as separate `Vec` elements; this function
/// joins their resolved ids with `, ` (`Selected option(s) a, b, c`).
/// The multi-select join shape is exercised by the test below.
///
/// `input_questions` carries both `id` and the option `label`/`id` map
/// so we can resolve the answer values (which arrive label-keyed from
/// the client) back to the option ids.
///
/// `annotations` carries per-question freeform notes (the text the user
/// typed when picking the freeform "Other" path or dismissing). When no
/// option labels resolve to ids and `notes` is non-empty, the result is
/// `Question <qid>: <raw_text>` (no `Selected option(s)` prefix).
///
/// Question order follows `input_questions`. Unanswered questions are
/// omitted -- only answered questions appear in the result.
pub fn format_id_keyed_accepted_tool_result(
    input_questions: &[super::Question],
    answers: &IndexMap<String, Vec<String>>,
    annotations: &Option<HashMap<String, QuestionAnnotation>>,
) -> String {
    let lines: Vec<String> = input_questions
        .iter()
        .filter_map(|q| {
            let qid = q.id.as_ref()?;
            let labels = answers.get(&q.question)?;
            // Each selected label is its own `Vec` element (the wire
            // format no longer joins labels with `", "`), so we look each
            // one up directly. No splitting, no ambiguity around labels
            // that contain commas or share substrings with other labels.
            let oids: Vec<&str> = labels
                .iter()
                .filter_map(|label| {
                    q.options
                        .iter()
                        .find(|o| &o.label == label)
                        .and_then(|o| o.id.as_deref())
                })
                .collect();
            if oids.is_empty() {
                // Freeform / dismissed: emit the raw text from the freeform
                // input directly after `Question <qid>: `.
                let notes = annotations
                    .as_ref()
                    .and_then(|m| m.get(&q.question))
                    .and_then(|a| a.notes.as_deref())
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())?;
                return Some(format!("Question {qid}: {notes}"));
            }
            Some(format!(
                "Question {qid}: Selected option(s) {}",
                oids.join(", ")
            ))
        })
        .collect();
    if lines.is_empty() {
        return "User questions responses:".to_string();
    }
    format!(
        "User questions responses:
{}",
        lines.join(
            "
"
        )
    )
}

// ── Path B: Chat about this (plan mode) ─────────────────────────────────

/// Format the tool result for Path B ("Chat about this" / respond-to-agent).
///
/// Iterates ALL original questions. Answered questions show their label;
/// unanswered questions show "(No answer provided)".
///
/// Whitespace is intentional:
/// - Lines 2-4 and "Questions asked:" have 4-space indentation.
/// - Question bullets have no indentation.
/// - Answer lines have 2-space indentation.
pub fn format_chat_about_this(
    questions: &[Question],
    partial_answers: &HashMap<String, String>,
) -> String {
    let question_lines: Vec<String> = questions
        .iter()
        .map(|q| {
            if let Some(answer) = partial_answers.get(&q.question) {
                format!("- \"{}\"\n  Answer: {}", q.question, answer)
            } else {
                format!("- \"{}\"\n  (No answer provided)", q.question)
            }
        })
        .collect();

    format!(
        "The user wants to clarify these questions.\n\
         \x20\x20\x20\x20This means they may have additional information, context or questions for you.\n\
         \x20\x20\x20\x20Take their response into account and then reformulate the questions if appropriate.\n\
         \x20\x20\x20\x20Start by asking them what they would like to clarify.\n\
         \n\
         \x20\x20\x20\x20Questions asked:\n\
         {}",
        question_lines.join("\n")
    )
}

// ── Path C: Skip interview (plan mode) ──────────────────────────────────

/// Format the tool result for Path C ("Skip interview and plan immediately").
///
/// Same per-question format as Path B, but different header and NO indentation.
pub fn format_skip_interview(
    questions: &[Question],
    partial_answers: &HashMap<String, String>,
) -> String {
    let question_lines: Vec<String> = questions
        .iter()
        .map(|q| {
            if let Some(answer) = partial_answers.get(&q.question) {
                format!("- \"{}\"\n  Answer: {}", q.question, answer)
            } else {
                format!("- \"{}\"\n  (No answer provided)", q.question)
            }
        })
        .collect();

    format!(
        "The user has indicated they have provided enough answers for the plan interview.\n\
         Stop asking clarifying questions and proceed to finish the plan with the information you have.\n\
         \n\
         Questions asked and answers provided:\n\
         {}",
        question_lines.join("\n")
    )
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::QuestionOption;
    use super::*;

    // -- Helpers --

    fn make_question(text: &str, labels: &[&str]) -> Question {
        Question {
            question: text.to_string(),
            options: labels
                .iter()
                .map(|l| QuestionOption {
                    label: l.to_string(),
                    description: format!("Description for {l}"),
                    preview: None,
                    id: None,
                })
                .collect(),
            multi_select: None,
            id: None,
        }
    }

    // ── Path A: format_accepted_tool_result ──────────────────────────────

    #[test]
    fn format_accepted_single_no_annotations() {
        let mut answers = IndexMap::new();
        answers.insert(
            "Which database?".to_string(),
            vec!["Redis (Recommended)".to_string()],
        );

        let result = format_accepted_tool_result(&answers, &None);
        assert_eq!(
            result,
            "User has answered your questions: \"Which database?\"=\"Redis (Recommended)\". You can now continue with the user's answers in mind."
        );
    }

    #[test]
    fn format_accepted_multiple_with_annotations() {
        let mut answers = IndexMap::new();
        answers.insert("Which database?".to_string(), vec!["Redis".to_string()]);
        answers.insert("Which framework?".to_string(), vec!["React".to_string()]);

        let mut anns = HashMap::new();
        anns.insert(
            "Which database?".to_string(),
            QuestionAnnotation {
                preview: Some("<div>redis preview</div>".to_string()),
                notes: None,
            },
        );
        anns.insert(
            "Which framework?".to_string(),
            QuestionAnnotation {
                preview: None,
                notes: Some("I prefer React hooks".to_string()),
            },
        );

        let result = format_accepted_tool_result(&answers, &Some(anns));
        assert_eq!(
            result,
            "User has answered your questions: \"Which database?\"=\"Redis\" selected preview:\n<div>redis preview</div>, \"Which framework?\"=\"React\" user notes: I prefer React hooks. You can now continue with the user's answers in mind."
        );
    }

    #[test]
    fn format_accepted_multi_select() {
        let mut answers = IndexMap::new();
        answers.insert(
            "Which features?".to_string(),
            vec!["Auth".to_string(), "Logging".to_string()],
        );

        let result = format_accepted_tool_result(&answers, &None);
        assert_eq!(
            result,
            "User has answered your questions: \"Which features?\"=\"Auth, Logging\". You can now continue with the user's answers in mind."
        );
    }

    #[test]
    fn format_accepted_freeform_only() {
        // Freeform-only: label is "Other", typed text in annotations.notes
        let mut answers = IndexMap::new();
        answers.insert("Which database?".to_string(), vec!["Other".to_string()]);

        let mut anns = HashMap::new();
        anns.insert(
            "Which database?".to_string(),
            QuestionAnnotation {
                preview: None,
                notes: Some("I want to use DynamoDB".to_string()),
            },
        );

        let result = format_accepted_tool_result(&answers, &Some(anns));
        assert_eq!(
            result,
            "User has answered your questions: \"Which database?\"=\"Other\" user notes: I want to use DynamoDB. You can now continue with the user's answers in mind."
        );
    }

    #[test]
    fn format_accepted_preview_and_notes() {
        let mut answers = IndexMap::new();
        answers.insert("Which layout?".to_string(), vec!["Grid".to_string()]);

        let mut anns = HashMap::new();
        anns.insert(
            "Which layout?".to_string(),
            QuestionAnnotation {
                preview: Some("<div class=\"grid\">...</div>".to_string()),
                notes: Some("Use CSS Grid for the main layout".to_string()),
            },
        );

        let result = format_accepted_tool_result(&answers, &Some(anns));
        assert_eq!(
            result,
            "User has answered your questions: \"Which layout?\"=\"Grid\" selected preview:\n<div class=\"grid\">...</div> user notes: Use CSS Grid for the main layout. You can now continue with the user's answers in mind."
        );
    }

    #[test]
    fn format_accepted_empty() {
        let answers = IndexMap::new();
        let result = format_accepted_tool_result(&answers, &None);
        assert_eq!(
            result,
            "User has answered your questions: . You can now continue with the user's answers in mind."
        );
    }

    #[test]
    fn format_accepted_partial() {
        // Only answered questions appear. Unanswered questions are omitted by the caller
        // (the answers IndexMap simply doesn't contain them).
        let mut answers = IndexMap::new();
        answers.insert("Which database?".to_string(), vec!["Redis".to_string()]);
        // "Which framework?" is unanswered => not in the map

        let result = format_accepted_tool_result(&answers, &None);
        assert_eq!(
            result,
            "User has answered your questions: \"Which database?\"=\"Redis\". You can now continue with the user's answers in mind."
        );
    }

    // ── Alternate id-keyed formatter tests ────────────────────
    //
    // Pin both result strings (single question and three questions) so any
    // drift in the formatter trips a deterministic failure. Update the
    // literal strings deliberately if the wire format ever changes.
    fn id_keyed_q(qid: &str, prompt: &str, opts: &[(&str, &str)]) -> super::super::Question {
        super::super::Question {
            question: prompt.to_string(),
            options: opts
                .iter()
                .map(|(oid, label)| super::super::QuestionOption {
                    label: (*label).to_string(),
                    description: (*label).to_string(),
                    preview: None,
                    id: Some((*oid).to_string()),
                })
                .collect(),
            multi_select: None,
            id: Some(qid.to_string()),
        }
    }

    #[test]
    fn format_id_keyed_single_question_single_select_matches_capture() {
        let questions = vec![id_keyed_q(
            "demo_pick",
            "This is a demo of the question tool. Which outcome should we pick?",
            &[
                ("a", "Option A — fast path (minimal scope)"),
                ("b", "Option B — thorough path (extra validation)"),
                ("c", "Option C — I'll decide later"),
            ],
        )];
        let mut answers = IndexMap::new();
        answers.insert(
            "This is a demo of the question tool. Which outcome should we pick?".to_string(),
            vec!["Option A — fast path (minimal scope)".to_string()],
        );
        let result = format_id_keyed_accepted_tool_result(&questions, &answers, &None);
        assert_eq!(
            result, "User questions responses:\nQuestion demo_pick: Selected option(s) a",
            "must match the id-keyed shape"
        );
    }

    #[test]
    fn format_id_keyed_three_questions_matches_capture() {
        let questions = vec![
            id_keyed_q(
                "q1",
                "Question 1 of 3: Morning drink?",
                &[("coffee", "Coffee"), ("tea", "Tea"), ("water", "Water")],
            ),
            id_keyed_q(
                "q2",
                "Question 2 of 3: How do you usually start a new task?",
                &[
                    ("read", "Read docs first"),
                    ("code", "Jump into code"),
                    ("plan", "Sketch a plan first"),
                ],
            ),
            id_keyed_q(
                "q3",
                "Question 3 of 3: What do you lean on before a push? (pick any)",
                &[
                    ("tests", "Tests"),
                    ("types", "Types"),
                    ("lint", "Lint/format"),
                ],
            ),
        ];
        let mut answers = IndexMap::new();
        answers.insert(
            "Question 1 of 3: Morning drink?".to_string(),
            vec!["Tea".to_string()],
        );
        answers.insert(
            "Question 2 of 3: How do you usually start a new task?".to_string(),
            vec!["Jump into code".to_string()],
        );
        answers.insert(
            "Question 3 of 3: What do you lean on before a push? (pick any)".to_string(),
            vec!["Tests".to_string()],
        );
        let result = format_id_keyed_accepted_tool_result(&questions, &answers, &None);
        assert_eq!(
            result,
            "User questions responses:\nQuestion q1: Selected option(s) tea\nQuestion q2: Selected option(s) code\nQuestion q3: Selected option(s) tests",
            "must match the id-keyed shape"
        );
    }

    #[test]
    fn format_id_keyed_multi_select_inferred_csv() {
        // Multi-select with multiple selections joins option ids with ", "
        // at format time. Each selected label arrives as its own Vec
        // element (the wire format no longer joins them).
        let questions = vec![id_keyed_q(
            "q3",
            "What do you lean on before a push? (pick any)",
            &[
                ("tests", "Tests"),
                ("types", "Types"),
                ("lint", "Lint/format"),
            ],
        )];
        let mut answers = IndexMap::new();
        answers.insert(
            "What do you lean on before a push? (pick any)".to_string(),
            vec![
                "Tests".to_string(),
                "Types".to_string(),
                "Lint/format".to_string(),
            ],
        );
        let result = format_id_keyed_accepted_tool_result(&questions, &answers, &None);
        assert_eq!(
            result,
            "User questions responses:\nQuestion q3: Selected option(s) tests, types, lint"
        );
    }

    #[test]
    fn format_id_keyed_unanswered_questions_are_omitted() {
        let questions = vec![
            id_keyed_q("q1", "First?", &[("a", "Apple"), ("b", "Banana")]),
            id_keyed_q("q2", "Second?", &[("x", "Xenon"), ("y", "Yttrium")]),
        ];
        let mut answers = IndexMap::new();
        answers.insert("First?".to_string(), vec!["Apple".to_string()]);
        let result = format_id_keyed_accepted_tool_result(&questions, &answers, &None);
        assert_eq!(
            result,
            "User questions responses:\nQuestion q1: Selected option(s) a"
        );
    }

    #[test]
    fn format_id_keyed_no_answers_emits_header_only() {
        let questions = vec![id_keyed_q("q1", "First?", &[("a", "Apple")])];
        let answers = IndexMap::new();
        let result = format_id_keyed_accepted_tool_result(&questions, &answers, &None);
        assert_eq!(result, "User questions responses:");
    }

    /// Freeform/dismiss:
    /// when the user dismisses or types freeform text instead of picking
    /// an option, the wire format emits the raw text directly after
    /// `Question <qid>: ` with NO `Selected option(s)` prefix. The pager
    /// sends `answers["..."] = ["Other"]` plus the typed text in
    /// `annotations[q].notes`; the formatter falls through to the notes.
    #[test]
    fn format_id_keyed_freeform_dismissal_uses_notes_without_selected_prefix() {
        let questions = vec![id_keyed_q(
            "random_one",
            "Pick one",
            &[
                ("ocean", "Ocean"),
                ("mountains", "Mountains"),
                ("city", "City"),
            ],
        )];
        let mut answers = IndexMap::new();
        answers.insert("Pick one".to_string(), vec!["Other".to_string()]);
        let mut anns = HashMap::new();
        anns.insert(
            "Pick one".to_string(),
            QuestionAnnotation {
                preview: None,
                notes: Some("nvm".to_string()),
            },
        );
        let result = format_id_keyed_accepted_tool_result(&questions, &answers, &Some(anns));
        assert_eq!(
            result, "User questions responses:\nQuestion random_one: nvm",
            "must match the id-keyed shape"
        );
    }

    /// Freeform with no notes (just `["Other"]` and no annotation) is
    /// indistinguishable from a no-answer to the formatter, so the
    /// question is dropped (matching the `oids.is_empty()` branch).
    #[test]
    fn format_id_keyed_freeform_without_notes_is_dropped() {
        let questions = vec![id_keyed_q("q1", "Pick", &[("a", "A")])];
        let mut answers = IndexMap::new();
        answers.insert("Pick".to_string(), vec!["Other".to_string()]);
        let result = format_id_keyed_accepted_tool_result(&questions, &answers, &None);
        assert_eq!(result, "User questions responses:");
    }

    #[test]
    fn format_accepted_special_chars() {
        // Quotes and newlines in labels appear verbatim (no escaping)
        let mut answers = IndexMap::new();
        answers.insert(
            "Which \"option\"?".to_string(),
            vec!["Option with\nnewline".to_string()],
        );

        let result = format_accepted_tool_result(&answers, &None);
        assert_eq!(
            result,
            "User has answered your questions: \"Which \"option\"?\"=\"Option with\nnewline\". You can now continue with the user's answers in mind."
        );
    }

    // ── Path B: format_chat_about_this ───────────────────────────────────

    #[test]
    fn format_chat_about_this_mixed() {
        let questions = vec![
            make_question("Which database?", &["Redis", "Postgres"]),
            make_question("Which framework?", &["React", "Vue"]),
        ];

        let mut partial = HashMap::new();
        partial.insert("Which database?".to_string(), "Redis".to_string());

        let result = format_chat_about_this(&questions, &partial);
        let expected = "\
The user wants to clarify these questions.
    This means they may have additional information, context or questions for you.
    Take their response into account and then reformulate the questions if appropriate.
    Start by asking them what they would like to clarify.

    Questions asked:
- \"Which database?\"
  Answer: Redis
- \"Which framework?\"
  (No answer provided)";

        assert_eq!(result, expected);
    }

    #[test]
    fn format_chat_about_this_all_answered() {
        let questions = vec![make_question("Which database?", &["Redis"])];

        let mut partial = HashMap::new();
        partial.insert("Which database?".to_string(), "Redis".to_string());

        let result = format_chat_about_this(&questions, &partial);
        assert!(result.contains("Answer: Redis"));
        assert!(!result.contains("(No answer provided)"));
    }

    #[test]
    fn format_chat_about_this_none_answered() {
        let questions = vec![make_question("Q1?", &["A"]), make_question("Q2?", &["B"])];

        let result = format_chat_about_this(&questions, &HashMap::new());
        assert!(result.contains("- \"Q1?\"\n  (No answer provided)"));
        assert!(result.contains("- \"Q2?\"\n  (No answer provided)"));
    }

    // ── Path C: format_skip_interview ────────────────────────────────────

    #[test]
    fn format_skip_interview_all_answered() {
        let questions = vec![
            make_question("Which database?", &["Redis", "Postgres"]),
            make_question("Which framework?", &["React", "Vue"]),
        ];

        let mut partial = HashMap::new();
        partial.insert("Which database?".to_string(), "Redis".to_string());
        partial.insert("Which framework?".to_string(), "React".to_string());

        let result = format_skip_interview(&questions, &partial);
        let expected = "\
The user has indicated they have provided enough answers for the plan interview.
Stop asking clarifying questions and proceed to finish the plan with the information you have.

Questions asked and answers provided:
- \"Which database?\"
  Answer: Redis
- \"Which framework?\"
  Answer: React";

        assert_eq!(result, expected);
    }

    #[test]
    fn format_skip_interview_mixed() {
        let questions = vec![
            make_question("Which database?", &["Redis"]),
            make_question("Which framework?", &["React"]),
        ];

        let mut partial = HashMap::new();
        partial.insert("Which database?".to_string(), "Redis".to_string());

        let result = format_skip_interview(&questions, &partial);
        assert!(result.contains("Answer: Redis"));
        assert!(result.contains("- \"Which framework?\"\n  (No answer provided)"));
    }

    #[test]
    fn format_skip_interview_no_indentation() {
        // Path C has NO indentation on any header lines (unlike Path B)
        let questions = vec![make_question("Q?", &["A"])];
        let result = format_skip_interview(&questions, &HashMap::new());

        // First line has no leading spaces
        let first_line = result.lines().next().unwrap();
        assert!(!first_line.starts_with(' '));

        // Second line has no leading spaces
        let second_line = result.lines().nth(1).unwrap();
        assert!(!second_line.starts_with(' '));

        // "Questions asked" line has no leading spaces
        assert!(result.contains("\nQuestions asked and answers provided:\n"));
    }

    // ── Path D: CANCEL_TEXT ─────────────────────────────────────────────

    #[test]
    fn format_cancel() {
        assert_eq!(
            CANCEL_TEXT,
            "User declined to answer the questions. Continue with the task using your best judgment, or ask different questions."
        );
    }

    #[test]
    fn format_no_operator() {
        assert_eq!(
            NO_OPERATOR_TEXT,
            "No user is available to answer questions in this non-interactive session. Continue with your best judgment; do not wait for clarification."
        );
    }
}
