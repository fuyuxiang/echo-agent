//! Requirement expressions for tool dependency validation.
//!
//! Three layers of evaluation, bottom-up:
//!
//! ```text
//! Expr<ToolRequirement>::eval(|req| req.eval(&ctx))       // top: walk requirement tree
//!   └─ Expr<ToolParamsRequirement>::eval(|pr| pr.check(params))  // mid: walk param conditions
//!       └─ Expr<serde_json::Value>::eval(|v| actual == v)        // leaf: value equality
//! ```
//!
//! `Expr<T>` is the generic boolean expression tree. Each domain type
//! (`ToolRequirement`, `ToolParamsRequirement`, `serde_json::Value`) plugs
//! in as the closure to `eval()`.

use crate::types::tool::ToolKind;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Expr<T> {
    Value(T),
    And(Vec<Expr<T>>),
    Or(Vec<Expr<T>>),
    Not(Box<Expr<T>>),
    True,
    False,
}

impl<T> From<T> for Expr<T> {
    fn from(value: T) -> Self {
        Self::Value(value)
    }
}

impl<T> Expr<T> {
    pub fn and(items: impl IntoIterator<Item = T>) -> Self {
        Self::And(items.into_iter().map(Self::Value).collect())
    }

    /// Evaluate the expression tree.
    ///
    /// `f` is called for each `Value(t)` node — it decides whether that
    /// leaf is true or false. `And`, `Or`, `Not` combine results as
    /// expected. `True`/`False` are constants.
    ///
    /// Usage:
    /// ```ignore
    /// // Top-level: evaluate a tool's requirements against the registry
    /// tool.requires_expr().eval(&|req| req.eval(&ctx))
    /// ```
    pub fn eval(&self, f: &impl Fn(&T) -> bool) -> bool {
        match self {
            Expr::True => true,
            Expr::False => false,
            Expr::Value(v) => f(v),
            Expr::And(items) => items.iter().all(|e| e.eval(f)),
            Expr::Or(items) => items.iter().any(|e| e.eval(f)),
            Expr::Not(inner) => !inner.eval(f),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ToolParamsRequirement {
    pub key: String,
    pub value: Expr<serde_json::Value>,
}

impl ToolParamsRequirement {
    /// Convenience: require `key == value` (equality check).
    pub fn new(key: &str, value: impl Into<serde_json::Value>) -> Self {
        Self {
            key: key.to_string(),
            value: Expr::Value(value.into()),
        }
    }

    /// Check this requirement against a JSON params object.
    ///
    /// Looks up `self.key` in `params`, then evaluates `self.value`
    /// as an expression over the actual value. For the common case
    /// (`Expr::Value(expected)`), this is just equality.
    pub fn check(&self, params: &serde_json::Value) -> bool {
        let actual = params.get(&self.key);
        self.value.eval(&|expected| actual == Some(expected))
    }
}

/// A tool in the proposed configuration, as seen by the evaluator.
pub struct ProposedTool<'a> {
    pub namespace: &'a str,
    pub id: &'a str,
    pub kind: ToolKind,
    pub params: &'a serde_json::Value,
    /// Input schema (JSON Schema) — used by `InputParam` to verify
    /// that a param exists and is visible in the schema.
    pub input_schema: Option<&'a serde_json::Value>,
}

/// The world a `ToolRequirement` evaluates against.
///
/// Built from the enabled tools + their params (from `ToolServerConfig`),
/// combined with static metadata (tool_kinds) from the `ToolRegistryBuilder`.
pub struct EvalContext<'a> {
    /// All enabled tools in the proposed configuration.
    pub tools: &'a [ProposedTool<'a>],
    /// Params of the tool whose `requires_expr` we're evaluating.
    /// Used by `IfParams` to check "our own" params.
    pub self_params: &'a serde_json::Value,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ToolRequirement {
    /// Require a specific tool (by namespace + id), optionally only when
    /// that tool's params satisfy `if_params`.
    Tool {
        namespace: String,
        id: String,
        if_params: Option<Expr<ToolParamsRequirement>>,
    },
    /// Require any tool of the given kind, optionally only when that
    /// tool's params satisfy `if_params`.
    ToolKind {
        kind: Expr<ToolKind>,
        /// When set, at least one tool of this kind must have params
        /// satisfying this expression.
        #[serde(skip_serializing_if = "Option::is_none")]
        if_params: Option<Expr<ToolParamsRequirement>>,
    },
    /// Conditional: only impose `requirement` when our own params
    /// satisfy `condition`.
    IfParams {
        condition: Expr<ToolParamsRequirement>,
        requirement: Box<ToolRequirement>,
    },
    /// Require that a tool of the given kind has a visible input param.
    ///
    /// Used when description templates reference `${{ params.<kind>.<param> }}`
    /// — ensures the param exists and isn't hidden/pinned by the client config.
    /// Validation fails if the param is missing from the tool's input schema.
    InputParam { kind: Expr<ToolKind>, param: String },
}

impl ToolRequirement {
    /// Evaluate this requirement against the proposed tool configuration.
    ///
    /// This is the closure passed to `Expr<ToolRequirement>::eval()`:
    /// ```ignore
    /// tool.requires_expr().eval(&|req| req.eval(&ctx))
    /// ```
    pub fn eval(&self, ctx: &EvalContext) -> bool {
        match self {
            // "Is tool `namespace:id` enabled, and do its params satisfy if_params?"
            ToolRequirement::Tool {
                namespace,
                id,
                if_params,
            } => ctx.tools.iter().any(|t| {
                t.namespace == namespace.as_str()
                    && t.id == id.as_str()
                    && if_params
                        .as_ref()
                        .is_none_or(|expr| expr.eval(&|pr| pr.check(t.params)))
            }),

            // "Is there any enabled tool of this kind, and do its params
            //  satisfy if_params?"
            ToolRequirement::ToolKind { kind, if_params } => ctx.tools.iter().any(|t| {
                kind.eval(&|k| &t.kind == k)
                    && if_params
                        .as_ref()
                        .is_none_or(|expr| expr.eval(&|pr| pr.check(t.params)))
            }),

            // "If MY params satisfy condition, check requirement.
            //  If condition is false → vacuously true (no requirement)."
            ToolRequirement::IfParams {
                condition,
                requirement,
            } => {
                let cond_met = condition.eval(&|pr| pr.check(ctx.self_params));
                if cond_met {
                    requirement.eval(ctx)
                } else {
                    true
                }
            }

            // "Does a tool of this kind have a visible input param with this name?"
            ToolRequirement::InputParam { kind, param } => ctx.tools.iter().any(|t| {
                kind.eval(&|k| &t.kind == k)
                    && t.input_schema
                        .as_ref()
                        .and_then(|s| s.get("properties"))
                        .and_then(|p| p.get(param.as_str()))
                        .is_some()
            }),
        }
    }
}

impl ToolRequirement {
    /// Conditional requirement: only impose `requirement` when our own
    /// params satisfy `condition`.
    pub fn if_params(condition: impl Into<Expr<ToolParamsRequirement>>, requirement: Self) -> Self {
        Self::IfParams {
            condition: condition.into(),
            requirement: Box::new(requirement),
        }
    }

    /// Require any tool of the given kind (no param constraints on the target).
    pub fn tool_kind(kind: impl Into<Expr<ToolKind>>) -> Self {
        Self::ToolKind {
            kind: kind.into(),
            if_params: None,
        }
    }

    /// Require any tool of the given kind whose params satisfy `params`.
    pub fn tool_kind_with_params(
        kind: impl Into<Expr<ToolKind>>,
        params: impl Into<Expr<ToolParamsRequirement>>,
    ) -> Self {
        Self::ToolKind {
            kind: kind.into(),
            if_params: Some(params.into()),
        }
    }

    /// Require that a tool of the given kind has a visible input param.
    pub fn input_param(kind: impl Into<Expr<ToolKind>>, param: &str) -> Self {
        Self::InputParam {
            kind: kind.into(),
            param: param.to_string(),
        }
    }

    /// Require a specific tool (by namespace + id).
    pub fn tool<
        T: crate::types::tool_metadata::ToolMetadata + echo_agent_tool_runtime::Tool + Default,
    >() -> Self {
        let t = T::default();
        Self::Tool {
            namespace: t.tool_namespace().to_string(),
            id: echo_agent_tool_runtime::Tool::id(&t).as_str().to_owned(),
            if_params: None,
        }
    }

    /// Require a specific tool, but only when its params satisfy `params`.
    pub fn tool_with_params<
        T: crate::types::tool_metadata::ToolMetadata + echo_agent_tool_runtime::Tool + Default,
    >(
        params: impl Into<Expr<ToolParamsRequirement>>,
    ) -> Self {
        let t = T::default();
        Self::Tool {
            namespace: t.tool_namespace().to_string(),
            id: echo_agent_tool_runtime::Tool::id(&t).as_str().to_owned(),
            if_params: Some(params.into()),
        }
    }
}
