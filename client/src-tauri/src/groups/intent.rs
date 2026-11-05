use regex::Regex;
use std::sync::OnceLock;

pub(super) fn discussion_only_request(text: &str) -> bool {
    struct Rules {
        code: Regex,
        mentions: Regex,
        clauses: Regex,
        only: Regex,
        first: Regex,
        no_write: Regex,
        execute: Regex,
    }
    static RULES: OnceLock<Rules> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        let patterns: serde_json::Value = serde_json::from_str(include_str!(
            "../../../src/features/groups/group-intent-rules.json"
        ))
        .unwrap();
        let rule = |key: &str| {
            regex::RegexBuilder::new(patterns[key].as_str().unwrap())
                .case_insensitive(true)
                .build()
                .unwrap()
        };
        Rules {
            code: Regex::new(r"```[\s\S]*?```|`[^`]*`").unwrap(),
            mentions: Regex::new(r"@[^\s，,。；;：:！!？?]+\s*").unwrap(),
            clauses: Regex::new(r"[，,。；;！!？?\n]").unwrap(),
            only: rule("only"),
            first: rule("first"),
            no_write: rule("noWrite"),
            execute: rule("execute"),
        }
    });
    let body = rules.code.replace_all(text, " ");
    let clauses = rules
        .clauses
        .split(&body)
        .map(|clause| rules.mentions.replace_all(clause, " ").trim().to_string())
        .filter(|clause| !clause.is_empty())
        .collect::<Vec<_>>();
    !clauses.iter().any(|clause| rules.execute.is_match(clause))
        && clauses.iter().any(|clause| {
            rules.only.is_match(clause)
                || rules.first.is_match(clause)
                || rules.no_write.is_match(clause)
        })
}
