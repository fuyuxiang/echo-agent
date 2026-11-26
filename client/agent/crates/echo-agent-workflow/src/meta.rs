use serde::{Deserialize, Serialize};

use crate::{
    MAX_PHASE_DETAIL_LEN, MAX_PHASE_TITLE_LEN, MAX_WORKFLOW_DESCRIPTION_LEN, MAX_WORKFLOW_NAME_LEN,
    MAX_WORKFLOW_PHASES, MAX_WORKFLOW_WHEN_TO_USE_LEN,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowMeta {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub phases: Vec<PhaseMeta>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseMeta {
    pub title: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("script failed to parse: {0}")]
    Parse(String),
    #[error("first statement must be `let meta = #{{ ... }};`")]
    MetaNotFirst,
    #[error("meta is not a valid map: {0}")]
    InvalidShape(String),
    #[error("{0} must be a non-empty string")]
    MissingField(&'static str),
    #[error("meta.name must be lowercase ASCII letters or digits separated by single hyphens")]
    InvalidName,
    #[error("{field} must be at most {max} UTF-8 bytes (got {actual})")]
    StringTooLong {
        field: String,
        max: usize,
        actual: usize,
    },
    #[error("meta.phases must contain at most {max} entries (got {actual})")]
    TooManyPhases { max: usize, actual: usize },
}

const META_PROBE_MAX_OPS: u64 = 100_000;

pub fn extract_meta(script: &str) -> Result<WorkflowMeta, MetaError> {
    if !first_statement_is_meta(script) {
        return Err(MetaError::MetaNotFirst);
    }

    let mut engine = rhai::Engine::new();
    engine.set_max_operations(META_PROBE_MAX_OPS);
    engine.set_max_expr_depths(128, 64);
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    engine.disable_symbol("eval");

    engine
        .compile(script)
        .map_err(|e| MetaError::Parse(crate::with_rhai_hint(e.to_string())))?;

    let mut scope = rhai::Scope::new();
    scope.push_dynamic("args", rhai::Dynamic::UNIT);

    let _ = engine.eval_with_scope::<rhai::Dynamic>(&mut scope, script);

    let meta_dyn = scope
        .get_value::<rhai::Map>("meta")
        .ok_or(MetaError::MetaNotFirst)?;

    let meta: WorkflowMeta = rhai::serde::from_dynamic(&meta_dyn.into())
        .map_err(|e| MetaError::InvalidShape(e.to_string()))?;

    validate_meta(&meta)?;
    Ok(meta)
}

fn validate_meta(meta: &WorkflowMeta) -> Result<(), MetaError> {
    if meta.name.trim().is_empty() {
        return Err(MetaError::MissingField("meta.name"));
    }
    validate_len("meta.name", &meta.name, MAX_WORKFLOW_NAME_LEN)?;
    if !valid_workflow_name(&meta.name) {
        return Err(MetaError::InvalidName);
    }

    if meta.description.trim().is_empty() {
        return Err(MetaError::MissingField("meta.description"));
    }
    validate_len(
        "meta.description",
        &meta.description,
        MAX_WORKFLOW_DESCRIPTION_LEN,
    )?;
    if let Some(when_to_use) = &meta.when_to_use {
        validate_len(
            "meta.when_to_use",
            when_to_use,
            MAX_WORKFLOW_WHEN_TO_USE_LEN,
        )?;
    }

    if meta.phases.len() > MAX_WORKFLOW_PHASES {
        return Err(MetaError::TooManyPhases {
            max: MAX_WORKFLOW_PHASES,
            actual: meta.phases.len(),
        });
    }
    let mut phase_titles = std::collections::HashSet::with_capacity(meta.phases.len());
    for (index, phase) in meta.phases.iter().enumerate() {
        if phase.title.trim().is_empty() {
            return Err(MetaError::MissingField("meta.phases[].title"));
        }
        if !phase_titles.insert(phase.title.as_str()) {
            return Err(MetaError::InvalidShape(format!(
                "duplicate meta.phases[].title: {:?}",
                phase.title
            )));
        }
        validate_len(
            &format!("meta.phases[{index}].title"),
            &phase.title,
            MAX_PHASE_TITLE_LEN,
        )?;
        if let Some(detail) = &phase.detail {
            validate_len(
                &format!("meta.phases[{index}].detail"),
                detail,
                MAX_PHASE_DETAIL_LEN,
            )?;
        }
    }
    Ok(())
}

fn validate_len(field: &str, value: &str, max: usize) -> Result<(), MetaError> {
    if value.len() > max {
        return Err(MetaError::StringTooLong {
            field: field.to_string(),
            max,
            actual: value.len(),
        });
    }
    Ok(())
}

fn valid_workflow_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes
            .last()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !bytes.windows(2).any(|pair| pair == b"--")
}

fn first_statement_is_meta(script: &str) -> bool {
    let mut rest = script;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.split_once('\n').map(|(_, r)| r).unwrap_or("");
            continue;
        }
        if let Some(after) = rest.strip_prefix("/*") {
            match after.split_once("*/") {
                Some((_, r)) => {
                    rest = r;
                    continue;
                }
                None => return false,
            }
        }
        break;
    }
    rest.starts_with("let meta") || rest.starts_with("const meta")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_valid_meta() {
        let meta = extract_meta(
            r#"
            // A workflow.
            let meta = #{
                name: "demo",
                description: "does things",
                phases: [#{ title: "Scan" }, #{ title: "Fix", detail: "apply" }],
            };
            let x = agent("hi");
            "#,
        )
        .expect("valid meta");
        assert_eq!(meta.name, "demo");
        assert_eq!(meta.phases.len(), 2);
        assert_eq!(meta.phases[1].detail.as_deref(), Some("apply"));
    }

    #[test]
    fn rejects_missing_meta() {
        assert!(matches!(
            extract_meta("let x = 1;"),
            Err(MetaError::MetaNotFirst)
        ));
    }

    #[test]
    fn rejects_meta_not_first() {
        assert!(matches!(
            extract_meta(r#"let x = 1; let meta = #{ name: "n", description: "d" };"#),
            Err(MetaError::MetaNotFirst)
        ));
    }

    #[test]
    fn rejects_empty_name() {
        assert!(matches!(
            extract_meta(r#"let meta = #{ name: "", description: "d" };"#),
            Err(MetaError::MissingField("meta.name"))
        ));
    }

    #[test]
    fn rejects_non_kebab_case_names() {
        for name in [
            "Upper",
            "under_score",
            "-leading",
            "trailing-",
            "two--hyphens",
            "-1",
        ] {
            let script = format!(r#"let meta = #{{ name: "{name}", description: "d" }};"#);
            assert!(
                matches!(extract_meta(&script), Err(MetaError::InvalidName)),
                "accepted invalid name {name:?}"
            );
        }
    }

    #[test]
    fn accepts_name_bounds() {
        let name = format!("1{}", "a".repeat(MAX_WORKFLOW_NAME_LEN - 1));
        let script = format!(r#"let meta = #{{ name: "{name}", description: "d" }};"#);
        assert_eq!(extract_meta(&script).unwrap().name, name);
    }

    #[test]
    fn rejects_oversized_meta_strings_and_phases() {
        let name = "a".repeat(MAX_WORKFLOW_NAME_LEN + 1);
        let script = format!(r#"let meta = #{{ name: "{name}", description: "d" }};"#);
        assert!(matches!(
            extract_meta(&script),
            Err(MetaError::StringTooLong { field, .. }) if field == "meta.name"
        ));

        let phases = std::iter::repeat_n(r#"#{ title: "phase" }"#, MAX_WORKFLOW_PHASES + 1)
            .collect::<Vec<_>>()
            .join(",");
        let script =
            format!(r#"let meta = #{{ name: "valid", description: "d", phases: [{phases}] }};"#);
        assert!(matches!(
            extract_meta(&script),
            Err(MetaError::TooManyPhases { .. })
        ));
    }

    #[test]
    fn rejects_empty_phase_title() {
        let error = extract_meta(
            r#"let meta = #{ name: "valid", description: "d", phases: [#{ title: " " }] };"#,
        );
        assert!(matches!(
            error,
            Err(MetaError::MissingField("meta.phases[].title"))
        ));
    }

    #[test]
    fn rejects_duplicate_phase_titles() {
        let error = extract_meta(
            r#"let meta = #{ name: "valid", description: "d", phases: [#{ title: "Scan" }, #{ title: "Scan" }] };"#,
        );
        assert!(matches!(
            error,
            Err(MetaError::InvalidShape(message)) if message.contains("duplicate meta.phases[].title")
        ));
    }

    #[test]
    fn rejects_unknown_meta_fields() {
        assert!(matches!(
            extract_meta(r#"let meta = #{ name: "valid", description: "d", typo: "ignored?" };"#,),
            Err(MetaError::InvalidShape(_))
        ));
        assert!(matches!(
            extract_meta(
                r#"let meta = #{ name: "valid", description: "d", phases: [#{ title: "p", typo: true }] };"#,
            ),
            Err(MetaError::InvalidShape(_))
        ));
    }

    #[test]
    fn rejects_syntax_errors() {
        assert!(matches!(
            extract_meta(r#"let meta = #{ name: "n", description: "d" }; fn {"#),
            Err(MetaError::Parse(_))
        ));
    }

    #[test]
    fn comments_before_meta_are_fine() {
        let meta = extract_meta(
            "/* header\ncomment */\n// line\nlet meta = #{ name: \"n\", description: \"d\" };",
        );
        assert!(meta.is_ok());
    }
}
