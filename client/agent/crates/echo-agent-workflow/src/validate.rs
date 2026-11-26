use crate::host::{AgentResult, BudgetState, WorkflowHostRequest};
use crate::{Journal, WorkflowOutcome, WorkflowRunParams, extract_meta, run_workflow};

#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub name: String,
    pub phases: usize,
    pub outcome_ok: bool,
    pub outcome_summary: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("meta: {0}")]
    Meta(#[from] crate::MetaError),
    #[error("dry-run: {0}")]
    Run(String),
}

pub fn default_probe_args() -> serde_json::Value {
    serde_json::json!({
        "objective": "stub objective",
        "query": "stub query",
        "breadth": 2,
        "target": "stub target",
        "skeptic_count": 1,
        "max_verify_attempts": 1,
        "baseline_commit": "",
        "test_command": "cargo test",
        "diff_summary": "stub diff",
        "since_commit": "abc123",
    })
}

pub fn validate_script(
    script: &str,
    args: Option<serde_json::Value>,
) -> Result<ValidationReport, ValidationError> {
    validate_script_with_agent_budget(script, args, crate::DEFAULT_AGENT_BUDGET)
}

pub fn validate_script_with_agent_budget(
    script: &str,
    args: Option<serde_json::Value>,
    agent_budget: u64,
) -> Result<ValidationReport, ValidationError> {
    let meta = extract_meta(script)?;

    let (host_tx, mut host_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = std::thread::spawn(move || {
        use WorkflowHostRequest as R;
        let mut agent_calls = 0u64;
        while let Some(req) = host_rx.blocking_recv() {
            match req {
                R::ReserveAgentCalls { count, reply } => {
                    let requested = agent_calls.saturating_add(count);
                    if requested > agent_budget {
                        let _ = reply.send(Err(crate::HostError::AgentCallQuotaExceeded {
                            requested,
                            maximum: agent_budget,
                        }));
                    } else {
                        agent_calls = requested;
                        let _ = reply.send(Ok(()));
                    }
                }
                R::ReleaseAgentCalls { count, reply } => {
                    agent_calls = agent_calls.saturating_sub(count);
                    let _ = reply.send(Ok(()));
                }
                R::SpawnAgent { reply, .. } => {
                    let _ = reply.send(Ok(AgentResult {
                        agent_id: "stub".into(),
                        success: true,
                        output: serde_json::json!({
                            "achieved": true,
                            "gaps": "",
                            "evidence": "stub evidence",
                            "questions": ["q1", "q2"],
                            "claims": [],
                            "uncertainties": [],
                            "verdicts": [],
                            "failures": ["test_a"],
                            "issues": "none",
                            "stub": true
                        }),
                        cancelled: false,
                        tokens_used: 1,
                        duration_ms: 1,
                    }));
                }
                R::BudgetQuery { reply } => {
                    let _ = reply.send(Ok(BudgetState {
                        total: None,
                        spent: 0,
                        reserved: 0,
                        remaining: None,
                    }));
                }
                R::RenderTemplate { reply, .. } => {
                    let _ = reply.send(Ok("stub template".into()));
                }
                R::WriteScratchFile { name, reply, .. } => {
                    let _ = reply.send(Ok(format!("scratch/{name}")));
                }
                R::ReadScratchFile { reply, .. } => {
                    let _ = reply.send(Ok("stub content".into()));
                }
                R::GitDiffSince { reply, .. } => {
                    let _ = reply.send(Ok("".into()));
                }
                R::Phase { .. } | R::Log { .. } | R::Telemetry { .. } => {}
            }
        }
    });

    let outcome = run_workflow(WorkflowRunParams {
        script: script.to_string(),
        args: args.unwrap_or_else(default_probe_args),
        journal: Journal::new(None),
        host_tx,
        cancel: tokio_util::sync::CancellationToken::new(),
        max_ops: 10_000_000,
    });
    drop(host);

    let (outcome_ok, outcome_summary) = match &outcome {
        WorkflowOutcome::Completed { result } => (
            true,
            format!("completed: {}", truncate(&result.to_string())),
        ),
        WorkflowOutcome::Paused { kind, message } => {
            (true, format!("paused ({kind:?}): {}", truncate(message)))
        }
        WorkflowOutcome::Failed { error } => (false, format!("failed: {error}")),
        other => (false, format!("{other:?}")),
    };
    if !outcome_ok {
        return Err(ValidationError::Run(outcome_summary));
    }

    Ok(ValidationReport {
        name: meta.name,
        phases: meta.phases.len(),
        outcome_ok,
        outcome_summary,
    })
}

fn truncate(s: &str) -> String {
    if s.chars().count() > 200 {
        let head: String = s.chars().take(200).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_script_passes() {
        let report = validate_script(
            "let meta = #{ name: \"t\", description: \"d\" };\nlet r = agent(\"work\");\ncomplete(r.output);",
            None,
        )
        .unwrap();
        assert_eq!(report.name, "t");
        assert!(report.outcome_ok);
    }

    #[test]
    fn missing_meta_fails() {
        assert!(matches!(
            validate_script("let x = 1;", None),
            Err(ValidationError::Meta(_))
        ));
    }

    #[test]
    fn default_probe_args_exercise_bundled_and_authoring_examples() {
        let args = default_probe_args();
        assert!(
            args["objective"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            args["query"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            args["target"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(args["breadth"].as_u64().is_some_and(|value| value >= 2));
        assert!(
            args["skeptic_count"]
                .as_u64()
                .is_some_and(|value| value >= 1)
        );
        assert!(
            args["max_verify_attempts"]
                .as_u64()
                .is_some_and(|value| value >= 1)
        );
    }

    #[test]
    fn runtime_misuse_fails() {
        let err = validate_script(
            "let meta = #{ name: \"t\", description: \"d\" };\nnot_a_host_fn();",
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::Run(_)), "{err}");
    }

    #[test]
    fn pause_counts_as_valid() {
        let report = validate_script(
            "let meta = #{ name: \"t\", description: \"d\" };\npause(\"verification\", \"needs input\");",
            None,
        )
        .unwrap();
        assert!(report.outcome_ok);
    }

    #[test]
    fn engine_limits_are_reported_as_dry_run_failures() {
        let script = format!(
            r#"
            let meta = #{{ name: "t", description: "d" }};
            let jobs = [];
            for i in 0..{} {{ jobs.push(#{{ prompt: "job" + i.to_string() }}); }}
            parallel(jobs);
            "#,
            crate::MAX_PARALLEL + 1
        );
        let error = validate_script(&script, None).unwrap_err().to_string();
        assert!(error.contains("parallel() accepts at most"), "got: {error}");

        let script = format!(
            r#"
            let meta = #{{ name: "t", description: "d" }};
            let jobs = [];
            for i in 0..{} {{ jobs.push(#{{ prompt: "job" + i.to_string() }}); }}
            parallel(jobs);
            agent("synthesize");
            "#,
            crate::DEFAULT_AGENT_BUDGET
        );
        let error = validate_script(&script, None).unwrap_err().to_string();
        assert!(
            error.contains(&format!(
                "agent budget exceeded: requested {}, maximum {}",
                crate::DEFAULT_AGENT_BUDGET + 1,
                crate::DEFAULT_AGENT_BUDGET
            )),
            "got: {error}"
        );
    }

    #[test]
    fn authoring_landmines_are_fixed_or_hinted() {
        let concat = |terms: usize| {
            let chain = (0..terms)
                .map(|i| format!("\"part{i}\""))
                .collect::<Vec<_>>()
                .join(" + ");
            format!(
                "let meta = #{{ name: \"t\", description: \"d\" }};\nlet p = {chain};\ncomplete(p);"
            )
        };
        assert!(validate_script(&concat(100), None).unwrap().outcome_ok);

        let hinted = |script: &str, expect: &[&str]| {
            let msg = validate_script(script, None).unwrap_err().to_string();
            for e in expect {
                assert!(msg.contains(e), "missing {e:?} in: {msg}");
            }
        };
        hinted(&concat(300), &["maximum complexity", "`+=` statements"]);
        hinted(
            "let meta = #{ name: \"t\", description: \"d\" };\nlet shared = false;\ncomplete(shared);",
            &["reserved keyword", "rename the variable"],
        );
        hinted(
            "let meta = #{ name: \"t\", description: \"d\" };\nlet s = \"abc\";\ncomplete(s[0].severity);",
            &["type 'char'", "indexing a string"],
        );
    }
}
