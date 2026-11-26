//! Parsing for named workflow launch arguments.

use echo_agent_sampling_types::{ReasoningEffort, ReasoningEffortOption};

pub(crate) struct NamedWorkflowArgs {
    pub args: serde_json::Value,
    pub objective: String,
    pub agent_budget: Option<u64>,
    pub effort: Option<ReasoningEffort>,
}

#[derive(serde::Deserialize)]
struct KnownLaunchArgs {
    #[serde(default)]
    objective: ObjectiveArg,
    #[serde(default)]
    query: ObjectiveArg,
    #[serde(default, deserialize_with = "deserialize_agent_budget")]
    agent_budget: Option<AgentBudget>,
    #[serde(default)]
    effort: Option<serde_json::Value>,
}

#[derive(Default, serde::Deserialize)]
#[serde(untagged)]
enum ObjectiveArg {
    Text(String),
    Other(serde_json::Value),
    #[default]
    Missing,
}

impl ObjectiveArg {
    fn resolve(self, query: Self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text),
            Self::Other(value) => {
                drop(value);
                None
            }
            Self::Missing => match query {
                Self::Text(text) => Some(text),
                Self::Other(value) => {
                    drop(value);
                    None
                }
                Self::Missing => None,
            },
        }
    }
}

struct AgentBudget(u64);

impl AgentBudget {
    fn try_new(value: u64) -> Result<Self, String> {
        if value == 0 {
            return Err("`agent_budget` must be a positive integer".to_string());
        }
        if value > echo_agent_workflow::MAX_AGENT_BUDGET {
            return Err(format!(
                "`agent_budget` must be at most {} agents",
                echo_agent_workflow::MAX_AGENT_BUDGET
            ));
        }
        Ok(Self(value))
    }

    fn into_inner(self) -> u64 {
        self.0
    }
}

fn deserialize_agent_budget<'de, D>(deserializer: D) -> Result<Option<AgentBudget>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    let budget = value
        .as_u64()
        .ok_or_else(|| serde::de::Error::custom("`agent_budget` must be a positive integer"))?;
    AgentBudget::try_new(budget)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

struct WorkflowEffort(ReasoningEffort);

impl WorkflowEffort {
    fn try_new(value: &str, effort_options: &[ReasoningEffortOption]) -> Result<Self, String> {
        if let Ok(effort) = value.parse::<ReasoningEffort>() {
            return Ok(Self(effort));
        }
        effort_options
            .iter()
            .find(|option| {
                option.id.eq_ignore_ascii_case(value) || option.label.eq_ignore_ascii_case(value)
            })
            .map(|option| Self(option.value))
            .ok_or_else(|| format!("invalid workflow `effort`: unknown reasoning effort '{value}'"))
    }

    fn into_inner(self) -> ReasoningEffort {
        self.0
    }
}

pub(crate) fn parse_named_workflow_args(
    input: &str,
    description: &str,
    effort_options: &[ReasoningEffortOption],
) -> Result<NamedWorkflowArgs, String> {
    let input = input.trim();
    let (flag_budget, flag_effort, input) = parse_named_workflow_flags(input, effort_options)?;
    if input.is_empty() {
        return Ok(NamedWorkflowArgs {
            args: serde_json::Value::Null,
            objective: description.to_string(),
            agent_budget: flag_budget,
            effort: flag_effort,
        });
    }
    if let Ok(args @ serde_json::Value::Object(_)) =
        serde_json::from_str::<serde_json::Value>(input)
    {
        let known: KnownLaunchArgs =
            serde_json::from_value(args.clone()).map_err(|error| error.to_string())?;
        let objective = known
            .objective
            .resolve(known.query)
            .unwrap_or_else(|| input.to_string());
        let json_budget = known.agent_budget.map(AgentBudget::into_inner);
        let json_effort = known
            .effort
            .map(|value| {
                let effort = value
                    .as_str()
                    .ok_or_else(|| "`effort` must be a string".to_string())?;
                WorkflowEffort::try_new(effort, effort_options).map(WorkflowEffort::into_inner)
            })
            .transpose()?;
        if flag_budget.is_some() && json_budget.is_some() {
            return Err("set `agent_budget` once, using either the slash flag or JSON".to_string());
        }
        if flag_effort.is_some() && json_effort.is_some() {
            return Err("set `effort` once, using either the slash flag or JSON".to_string());
        }
        return Ok(NamedWorkflowArgs {
            args,
            objective,
            agent_budget: flag_budget.or(json_budget),
            effort: flag_effort.or(json_effort),
        });
    }
    Ok(NamedWorkflowArgs {
        args: serde_json::json!({ "query": input, "objective": input }),
        objective: input.to_string(),
        agent_budget: flag_budget,
        effort: flag_effort,
    })
}

fn parse_named_workflow_flags<'a>(
    mut input: &'a str,
    effort_options: &[ReasoningEffortOption],
) -> Result<(Option<u64>, Option<ReasoningEffort>, &'a str), String> {
    let mut agent_budget = None;
    let mut effort = None;
    loop {
        if let Some((value, remaining)) = parse_leading_arg(input, "agent-budget")? {
            if agent_budget.is_some() {
                return Err("set `--agent-budget` once".to_string());
            }
            let budget = value
                .parse::<u64>()
                .map_err(|_| "`--agent-budget` must be a positive integer".to_string())?;
            agent_budget = Some(AgentBudget::try_new(budget)?.into_inner());
            input = remaining;
        } else if let Some((value, remaining)) = parse_leading_arg(input, "effort")? {
            if effort.is_some() {
                return Err("set `--effort` once".to_string());
            }
            effort = Some(WorkflowEffort::try_new(value, effort_options)?.into_inner());
            input = remaining;
        } else {
            return Ok((agent_budget, effort, input));
        }
    }
}

fn parse_leading_arg<'a>(input: &'a str, name: &str) -> Result<Option<(&'a str, &'a str)>, String> {
    let flag = format!("--{name}");
    let Some(rest) = input.strip_prefix(&flag) else {
        return Ok(None);
    };
    let value_input = if let Some(rest) = rest.strip_prefix('=') {
        rest
    } else if rest.is_empty() {
        return Err(format!("`{flag}` requires a value"));
    } else if rest.chars().next().is_some_and(char::is_whitespace) {
        rest.trim_start()
    } else {
        return Ok(None);
    };
    if value_input.is_empty() {
        return Err(format!("`{flag}` requires a value"));
    }
    let (value, remaining) = value_input
        .split_once(char::is_whitespace)
        .map_or((value_input, ""), |(value, input)| {
            (value, input.trim_start())
        });
    Ok(Some((value, remaining)))
}

#[cfg(test)]
mod named_workflow_args_tests {
    use super::{
        NamedWorkflowArgs, ReasoningEffort, ReasoningEffortOption, parse_leading_arg,
        parse_named_workflow_args as parse_with_effort_options,
    };

    fn parse_named_workflow_args(
        input: &str,
        description: &str,
    ) -> Result<NamedWorkflowArgs, String> {
        parse_with_effort_options(input, description, &[])
    }

    fn remapped_effort_options() -> Vec<ReasoningEffortOption> {
        vec![ReasoningEffortOption {
            id: "deep".to_string(),
            value: ReasoningEffort::Xhigh,
            label: "Deep".to_string(),
            description: None,
            default: false,
        }]
    }

    #[test]
    fn typed_json_fields_preserve_objective_precedence() {
        let parsed = parse_named_workflow_args(
            r#"{"objective":"primary","query":"alias","extra":{"nested":true}}"#,
            "fallback",
        )
        .expect("valid args");
        assert_eq!(parsed.objective, "primary");
        assert_eq!(
            parsed.args,
            serde_json::json!({
                "objective": "primary",
                "query": "alias",
                "extra": {"nested": true},
            })
        );

        let alias =
            parse_named_workflow_args(r#"{"query":"alias"}"#, "fallback").expect("valid alias");
        assert_eq!(alias.objective, "alias");

        let non_text_objective =
            parse_named_workflow_args(r#"{"objective":null,"query":"alias"}"#, "fallback")
                .expect("valid non-text objective");
        assert_eq!(
            non_text_objective.objective,
            r#"{"objective":null,"query":"alias"}"#
        );
    }

    #[test]
    fn json_promotes_agent_budget_and_preserves_args() {
        let parsed = parse_named_workflow_args(
            r#"{"query":"review this","agent_budget":256,"target":"main"}"#,
            "fallback",
        )
        .expect("valid args");
        assert_eq!(parsed.objective, "review this");
        assert_eq!(parsed.agent_budget, Some(256));
        assert_eq!(parsed.effort, None);
        assert_eq!(
            parsed.args,
            serde_json::json!({
                "query": "review this",
                "agent_budget": 256,
                "target": "main",
            })
        );
    }

    #[test]
    fn slash_flag_promotes_budget_for_json_or_plain_args() {
        let json = parse_named_workflow_args(
            r#"--agent-budget 64 {"objective":"audit","target":"main"}"#,
            "fallback",
        )
        .expect("valid JSON args");
        assert_eq!(json.agent_budget, Some(64));
        assert_eq!(json.objective, "audit");
        assert_eq!(
            json.args,
            serde_json::json!({"objective": "audit", "target": "main"})
        );

        let plain = parse_named_workflow_args("--agent-budget=32 audit the release", "fallback")
            .expect("valid plain args");
        assert_eq!(plain.agent_budget, Some(32));
        assert_eq!(plain.objective, "audit the release");
        assert_eq!(
            plain.args,
            serde_json::json!({
                "query": "audit the release",
                "objective": "audit the release",
            })
        );
    }

    #[test]
    fn json_or_slash_flags_promote_effort() {
        let json =
            parse_named_workflow_args(r#"{"objective":"audit","effort":"HIGH"}"#, "fallback")
                .expect("valid JSON effort");
        assert_eq!(json.effort, Some(ReasoningEffort::High));

        for input in [
            "--effort medium --agent-budget 64 audit the release",
            "--agent-budget 64 --effort=medium audit the release",
        ] {
            let flags = parse_named_workflow_args(input, "fallback").expect("valid slash flags");
            assert_eq!(flags.effort, Some(ReasoningEffort::Medium));
            assert_eq!(flags.agent_budget, Some(64));
            assert_eq!(flags.objective, "audit the release");
        }
    }

    #[test]
    fn current_model_effort_aliases_canonicalize_for_all_flag_orders() {
        let options = remapped_effort_options();
        for input in [
            "--effort deep --agent-budget 64 audit",
            "--agent-budget 64 --effort Deep audit",
            "--effort=xhigh --agent-budget=64 audit",
            "--agent-budget=64 --effort=xhigh audit",
        ] {
            let parsed = parse_with_effort_options(input, "fallback", &options)
                .unwrap_or_else(|error| panic!("input={input:?}, error={error}"));
            assert_eq!(
                parsed.effort,
                Some(ReasoningEffort::Xhigh),
                "input={input:?}"
            );
            assert_eq!(parsed.agent_budget, Some(64), "input={input:?}");
            assert_eq!(parsed.objective, "audit", "input={input:?}");
        }

        let json = parse_with_effort_options(
            r#"{"objective":"audit","effort":"Deep"}"#,
            "fallback",
            &options,
        )
        .expect("current-model label must canonicalize");
        assert_eq!(json.effort, Some(ReasoningEffort::Xhigh));

        for input in ["--effort turbo audit", r#"{"effort":"turbo"}"#] {
            let error = parse_with_effort_options(input, "fallback", &options)
                .err()
                .unwrap_or_else(|| panic!("input={input:?} should fail"));
            assert!(error.contains("invalid workflow `effort`"), "{error}");
        }
    }

    #[test]
    fn absent_budget_keeps_default_launch_behavior() {
        let empty = parse_named_workflow_args("", "fallback").expect("empty args");
        assert_eq!(empty.agent_budget, None);
        assert_eq!(empty.effort, None);
        assert_eq!(empty.objective, "fallback");
        assert_eq!(empty.args, serde_json::Value::Null);

        let plain = parse_named_workflow_args("audit", "fallback").expect("plain args");
        assert_eq!(plain.agent_budget, None);
        assert_eq!(plain.objective, "audit");
    }

    #[test]
    fn invalid_budgets_are_rejected() {
        for (input, expected) in [
            (r#"{"agent_budget":0}"#, "positive integer"),
            (r#"{"agent_budget":1025}"#, "at most 1024"),
            (r#"{"agent_budget":"64"}"#, "positive integer"),
            ("--agent-budget nope audit", "positive integer"),
            ("--agent-budget", "requires a value"),
            (r#"{"effort":"turbo"}"#, "invalid workflow `effort`"),
            (r#"{"effort":3}"#, "must be a string"),
            ("--effort turbo audit", "invalid workflow `effort`"),
            ("--effort", "requires a value"),
        ] {
            let error = parse_named_workflow_args(input, "fallback")
                .err()
                .unwrap_or_else(|| panic!("{input:?} should fail"));
            assert!(error.contains(expected), "input={input:?}, error={error}");
        }
    }

    #[test]
    fn duplicate_flag_and_json_launch_fields_are_rejected() {
        let budget =
            parse_named_workflow_args(r#"--agent-budget 64 {"agent_budget":128}"#, "fallback")
                .err()
                .expect("duplicate budget must fail");
        assert!(budget.contains("set `agent_budget` once"), "{budget}");

        let effort = parse_named_workflow_args(r#"--effort low {"effort":"high"}"#, "fallback")
            .err()
            .expect("duplicate effort must fail");
        assert!(effort.contains("set `effort` once"), "{effort}");
    }

    #[test]
    fn duplicate_slash_effort_flags_are_rejected() {
        for input in [
            "--effort low --effort high audit",
            "--effort=low --effort=high audit",
        ] {
            let error = parse_named_workflow_args(input, "fallback")
                .err()
                .unwrap_or_else(|| panic!("{input:?} should fail"));
            assert_eq!(error, "set `--effort` once", "input={input:?}");
        }
    }

    #[test]
    fn duplicate_slash_budget_flags_are_rejected() {
        for input in [
            "--agent-budget 32 --agent-budget 64 audit",
            "--agent-budget 32 --agent-budget=64 audit",
            "--agent-budget=32 --agent-budget 64 audit",
            "--agent-budget=32 --agent-budget=64 audit",
        ] {
            let error = parse_named_workflow_args(input, "fallback")
                .err()
                .unwrap_or_else(|| panic!("{input:?} should fail"));
            assert_eq!(error, "set `--agent-budget` once", "input={input:?}");
        }
    }

    #[test]
    fn whitespace_delimits_slash_budget_value() {
        for whitespace in ["\t", "\n", "\r\n", "\u{2003}"] {
            let input = format!("--agent-budget{whitespace}64{whitespace}audit");
            let parsed = parse_named_workflow_args(&input, "fallback")
                .unwrap_or_else(|error| panic!("input={input:?}, error={error}"));
            assert_eq!(parsed.agent_budget, Some(64), "input={input:?}");
            assert_eq!(parsed.objective, "audit", "input={input:?}");
        }
    }

    #[test]
    fn generic_leading_arg_supports_equals_whitespace_and_missing_values() {
        assert_eq!(
            parse_leading_arg("--effort=high audit", "effort").expect("valid equals arg"),
            Some(("high", "audit"))
        );
        for whitespace in [" ", "\t", "\n", "\r\n", "\u{2003}"] {
            let input = format!("--effort{whitespace}high{whitespace}audit");
            assert_eq!(
                parse_leading_arg(&input, "effort").expect("valid whitespace arg"),
                Some(("high", "audit")),
                "input={input:?}"
            );
        }
        assert_eq!(
            parse_leading_arg("--unknown value", "effort").expect("different flag"),
            None
        );
        assert_eq!(
            parse_leading_arg("--effort", "effort").expect_err("missing value"),
            "`--effort` requires a value"
        );
        assert_eq!(
            parse_leading_arg("--effort=", "effort").expect_err("missing equals value"),
            "`--effort` requires a value"
        );
    }
}
