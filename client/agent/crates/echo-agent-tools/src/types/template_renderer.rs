//! Pre-built template renderer for tool/param name resolution and
//! host-shell branching.
//!
//! `TemplateRenderer` is built once at finalize time with the tool kind →
//! client-facing name mappings and stored in Resources. Tools and reminders
//! call `render()` at runtime to resolve `${{ tools.by_kind.read }}`,
//! `${{ params.edit.old_string }}`, etc. Host-shell flags
//! (`is_windows`, `shell_uses_semicolon`, `has_unix_utilities`) are
//! computed once at construction so templates can branch on the runtime
//! environment without per-tool plumbing.
//!
//! Skills are **not** part of this renderer — they are passed to the Skill
//! tool at construction time and rendered in its description at finalize time.
//!
//! # Usage
//!
//! ```ignore
//! let renderer = resources.get::<TemplateRenderer>().unwrap();
//!
//! // Resolve a tool name
//! let name = renderer.render("${{ tools.by_kind.read }}")?;
//!
//! // Resolve a param name
//! let param = renderer.render("${{ params.edit.old_string }}")?;
//!
//! // Branch on the host shell
//! let msg = renderer.render(
//!     "${%- if has_unix_utilities %}use grep${%- else %}grep is unavailable${%- endif %}"
//! )?;
//! ```

use std::collections::HashMap;

use crate::types::description::make_desc_env;
use crate::types::tool::ToolKind;

/// Nested map under `tools` — holds `by_kind` so templates write
/// `${{ tools.by_kind.read }}` instead of `${{ tools.read }}`.
#[derive(Debug, Clone, serde::Serialize)]
struct ToolsContext {
    /// ToolKind → client-facing tool name.
    by_kind: HashMap<ToolKind, String>,
}

/// The data MiniJinja sees at render time.
///
/// `tools.by_kind` and `params` keys are snake_case (serde serializes
/// `ToolKind::Read` → `"read"`), so templates write
/// `${{ tools.by_kind.read }}` / `${{ params.edit.old_string }}`.
///
/// Shell flags are computed once in [`TemplateRenderer::new`] from
/// [`echo_agent_config::shell`].
#[derive(Debug, Clone, serde::Serialize)]
struct TemplateContext {
    tools: ToolsContext,
    params: HashMap<String, HashMap<String, String>>,
    /// `cfg!(not(unix))`.
    is_windows: bool,
    /// `powershell.exe` 5.1 and `cmd.exe` chain with `;`; everything else
    /// with `&&`. Used by templates that document command chaining.
    shell_uses_semicolon: bool,
    /// Whether `grep`, `head`, `tail`, `sed`, `awk`, `find` are usable
    /// from the active shell. False on Windows + PowerShell / cmd.exe;
    /// true everywhere else. Tool descriptions branch on this to swap
    /// Unix-centric guidance for PowerShell-aware guidance.
    has_unix_utilities: bool,
    /// Whether the client delivers system reminders (e.g. completion
    /// notifications for backgrounded commands/subagents) to the model.
    /// Descriptions that promise "you are notified on completion" branch
    /// on this so the promise is only made when it can be kept. Defaults
    /// to `true` (prod CLI behavior).
    system_reminders_enabled: bool,
}

/// Shared render implementation: fast-path check + MiniJinja render.
fn render_with_env(
    template: &str,
    ctx: &impl serde::Serialize,
) -> Result<String, TemplateRenderError> {
    if !template.contains("${{") && !template.contains("${%") {
        return Ok(template.to_string());
    }
    let env = make_desc_env();
    env.render_str(template, ctx)
        .map_err(|source| TemplateRenderError {
            template: template.to_string(),
            source,
        })
}

/// Error returned when a template fails to render.
///
/// Wraps the underlying MiniJinja error. Callers that want the old
/// silent-fallback behavior can use `.unwrap_or_else(|_| ...)`.
#[derive(Debug)]
pub struct TemplateRenderError {
    template: String,
    source: minijinja::Error,
}

impl TemplateRenderError {
    /// The raw template that failed to render.
    pub fn template(&self) -> &str {
        &self.template
    }
}

impl std::fmt::Display for TemplateRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "template render failed: {} (template: {:?})",
            self.source, self.template
        )
    }
}

impl std::error::Error for TemplateRenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Pre-built template renderer stored in Resources.
///
/// Strip unrendered template markers (`${{ … }}` and `${% … %}`) from `raw`.
///
/// Last-resort fallback for when MiniJinja rendering fails: the model must
/// never see raw template syntax, so the offending spans are dropped
/// entirely. The result may read slightly awkwardly around the removed
/// spans, but it is always plain prose.
/// Render-failure fallback: strip template markers so the model never sees
/// raw syntax. A render failure always indicates a template bug, so debug
/// builds (tests, local dev) panic loudly instead of degrading; release
/// builds log and degrade gracefully.
///
/// Not for the *intentional* pre-finalize sanitize path
/// (`sanitized_description_template`), which calls
/// [`strip_template_markers`] directly.
#[track_caller]
pub fn strip_markers_on_render_failure(raw: &str, err: &TemplateRenderError) -> String {
    debug_assert!(
        false,
        "description template failed to render — fix the template instead of \
         relying on the marker-strip fallback: {err}"
    );
    tracing::warn!("Description template render failed, stripping markers: {err}");
    strip_template_markers(raw)
}

pub fn strip_template_markers(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let close: Option<usize> = if after.starts_with('{') {
            after.find("}}").map(|i| i + 2)
        } else if after.starts_with('%') {
            after.find("%}").map(|i| i + 2)
        } else {
            None
        };
        match close {
            Some(end_in_after) => {
                out.push_str(&rest[..start]);
                rest = &rest[start + 2 + end_in_after..];
            }
            None => {
                // `${` that is not a marker (or unterminated) — keep as-is.
                out.push_str(&rest[..start + 2]);
                rest = &rest[start + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Created once at finalize time and available to all tools and reminders
/// via `resources.get::<TemplateRenderer>()`. Uses MiniJinja with custom
/// `${{ }}` / `${%  %}` delimiters to avoid collisions with literal `{{ }}`
/// in tool descriptions and error messages. Context is baked in at
/// construction and cannot be modified afterwards.
#[derive(Clone)]
pub struct TemplateRenderer {
    ctx: TemplateContext,
}

impl TemplateRenderer {
    /// Create a new renderer from the finalized kind → name mappings.
    ///
    /// `tools`: `ToolKind` → client-facing tool name (e.g., `Read → "read_file"`).
    /// `params`: `ToolKind` → { canonical param → client-facing param }.
    ///
    /// The `params` map keys are converted to snake_case strings to match
    /// how `tools.by_kind` keys appear in templates.
    pub fn new(
        tools: HashMap<ToolKind, String>,
        params: HashMap<ToolKind, HashMap<String, String>>,
    ) -> Self {
        // ToolKind keys → snake_case strings so templates can write
        // `${{ params.edit.old_string }}`.
        let params = params
            .into_iter()
            .filter_map(|(kind, map)| {
                let key = serde_json::to_value(kind).ok()?.as_str()?.to_string();
                Some((key, map))
            })
            .collect();

        Self {
            ctx: TemplateContext {
                tools: ToolsContext { by_kind: tools },
                params,
                is_windows: cfg!(not(unix)),
                // `chain_separator()` returns `"&&"` on Unix, so the
                // comparison is naturally false there — no cfg guard needed.
                shell_uses_semicolon: echo_agent_config::shell::chain_separator() == ";",
                has_unix_utilities: echo_agent_config::shell::has_unix_utilities(),
                system_reminders_enabled: true,
            },
        }
    }

    /// Override whether templates see `system_reminders_enabled` as true.
    /// Called at finalize time with the client's setting; every other
    /// construction site keeps the default (`true`).
    #[must_use]
    pub fn with_system_reminders_enabled(mut self, enabled: bool) -> Self {
        self.ctx.system_reminders_enabled = enabled;
        self
    }

    /// Render a template string with the full context.
    ///
    /// Template syntax:
    /// - `${{ tools.by_kind.read }}` — resolves to the client-facing Read tool name
    /// - `${{ params.edit.old_string }}` — resolves to the client-facing param name
    /// - `${%- if tools.by_kind.search %}...${%- endif %}` — conditional sections
    ///
    /// Returns the raw template unchanged (without error) if it contains
    /// no template markers (`${{` or `${%`).
    pub fn render(&self, template: &str) -> Result<String, TemplateRenderError> {
        render_with_env(template, &self.ctx)
    }

    /// Return the finalized canonical-to-client parameter names by tool kind.
    pub fn param_names(&self) -> HashMap<ToolKind, HashMap<String, String>> {
        self.ctx
            .params
            .iter()
            .filter_map(|(kind, names)| {
                serde_json::from_value(serde_json::Value::String(kind.clone()))
                    .ok()
                    .map(|kind| (kind, names.clone()))
            })
            .collect()
    }

    /// Render `${{ ... }}` placeholders in every `description` string within a
    /// JSON Schema, in place — recursing into nested objects, array `items`, and
    /// `$defs`. Property keys are remapped separately; this resolves
    /// descriptions that reference another param/tool via
    /// `${{ params.<kind>.<param> }}` or `${{ tools.by_kind.<kind> }}`.
    /// Untemplated descriptions are left as-is; a render failure logs and
    /// strips the template markers so raw syntax never reaches the model.
    pub fn render_schema_descriptions(&self, schema: &mut serde_json::Value) {
        match schema {
            serde_json::Value::Object(map) => {
                let rendered = match map.get("description") {
                    Some(serde_json::Value::String(desc))
                        if desc.contains("${{") || desc.contains("${%") =>
                    {
                        match self.render(desc) {
                            Ok(r) => Some(r),
                            Err(e) => Some(strip_markers_on_render_failure(desc, &e)),
                        }
                    }
                    _ => None,
                };
                if let Some(rendered) = rendered {
                    map.insert(
                        "description".to_string(),
                        serde_json::Value::String(rendered),
                    );
                }
                for value in map.values_mut() {
                    self.render_schema_descriptions(value);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items.iter_mut() {
                    self.render_schema_descriptions(item);
                }
            }
            _ => {}
        }
    }

    /// Returns the client-facing tool name for the given `ToolKind` if a
    /// tool of that kind is registered in the finalized toolset.
    ///
    /// Use this when you need a structural answer to "does the active
    /// toolset expose a tool of kind X?" — the kind→name map is the
    /// single source of truth, populated at `FinalizedToolset` build time
    /// from each tool's `kind()`.
    pub fn tool_for_kind(&self, kind: ToolKind) -> Option<&str> {
        self.ctx.tools.by_kind.get(&kind).map(String::as_str)
    }

    /// The client-facing name of a canonical parameter on the tool of `kind`,
    /// or `None` when no tool of that kind exposes that parameter in the
    /// finalized toolset.
    ///
    /// This is the param-map twin of [`Self::tool_for_kind`]: it resolves the
    /// same `${{ params.<kind>.<param> }}` mapping that [`Self::render`] would,
    /// but eagerly and as an `Option`.
    pub fn param_for_kind<'a>(&'a self, kind: ToolKind, canonical: &str) -> Option<&'a str> {
        let key_value = serde_json::to_value(kind).ok()?;
        let key = key_value.as_str()?;
        self.ctx.params.get(key)?.get(canonical).map(String::as_str)
    }

    /// Acquire `TemplateRenderer` from shared resources
    /// and render a template in one call.
    ///
    /// Replaces the common lock → require → render → map_err boilerplate:
    /// ```ignore
    /// let name = TemplateRenderer::resolve(&resources, "${{ params.edit.replace_all }}").await?;
    /// ```
    pub async fn resolve(
        resources: &crate::types::resources::SharedResources,
        template: &str,
    ) -> Result<String, echo_agent_tool_runtime::ToolError> {
        let res = resources.lock().await;
        let renderer = res.require::<Self>()?;
        renderer
            .render(template)
            .map_err(|e| echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string()))
    }

    /// Look up a tool's client-facing name by kind from shared resources.
    ///
    /// Convenience wrapper around `lock → get → tool_for_kind`. Returns
    /// `None` when no `TemplateRenderer` is in resources or no tool of
    /// that kind is registered.
    /// ```ignore
    /// let name = TemplateRenderer::resolve_tool_name(&resources, ToolKind::KillTaskAction).await;
    /// ```
    pub async fn resolve_tool_name(
        resources: &crate::types::resources::SharedResources,
        kind: ToolKind,
    ) -> Option<String> {
        let res = resources.lock().await;
        res.get::<Self>()
            .and_then(|r| r.tool_for_kind(kind).map(str::to_string))
    }

    /// Render a template with the renderer's tool context **plus** arbitrary
    /// extra placeholders.
    ///
    /// `placeholders` is a JSON object whose keys are merged at the top
    /// level alongside `tools` and `params`:
    ///
    /// ```text
    /// ${{ tools.by_kind.read }}   ← from renderer
    /// ${{ os_name }}              ← from placeholders
    /// ${%- if memory_enabled %}   ← from placeholders (bool)
    /// ```
    pub fn render_with_extra(
        &self,
        template: &str,
        placeholders: &serde_json::Value,
    ) -> Result<String, TemplateRenderError> {
        if !template.contains("${{") && !template.contains("${%") {
            return Ok(template.to_string());
        }
        // Merge: start with renderer context, overlay caller placeholders.
        let mut base = serde_json::to_value(&self.ctx).unwrap_or_default();
        if let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), placeholders.as_object())
        {
            for (k, v) in extra_obj {
                base_obj.insert(k.clone(), v.clone());
            }
        }
        let env = make_desc_env();
        env.render_str(template, base)
            .map_err(|source| TemplateRenderError {
                template: template.to_string(),
                source,
            })
    }
}

impl std::fmt::Debug for TemplateRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateRenderer")
            .field("tools", &self.ctx.tools.by_kind.len())
            .field("params", &self.ctx.params.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_template_markers_removes_interpolations_and_tags() {
        let raw = "Use ${{ tools.by_kind.read }} first.${%- if tools.by_kind.edit %} Then ${{ tools.by_kind.edit }}.${%- endif %}";
        assert_eq!(strip_template_markers(raw), "Use  first. Then .");
    }

    #[test]
    fn strip_template_markers_keeps_plain_text_and_literal_dollar_brace() {
        assert_eq!(strip_template_markers("no markers here"), "no markers here");
        // `${` not followed by `{`/`%` is preserved (e.g. shell `${VAR}`).
        assert_eq!(strip_template_markers("echo ${VAR}"), "echo ${VAR}");
        // Unterminated marker is preserved rather than eating the rest.
        assert_eq!(strip_template_markers("broken ${{ tail"), "broken ${{ tail");
    }

    fn make_renderer(
        tools: &[(ToolKind, &str)],
        params: &[(ToolKind, &[(&str, &str)])],
    ) -> TemplateRenderer {
        let tool_map: HashMap<ToolKind, String> =
            tools.iter().map(|(k, n)| (*k, n.to_string())).collect();
        let param_map: HashMap<ToolKind, HashMap<String, String>> = params
            .iter()
            .map(|(k, ps)| {
                let m = ps
                    .iter()
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .collect();
                (*k, m)
            })
            .collect();
        TemplateRenderer::new(tool_map, param_map)
    }

    #[test]
    fn render_tool_name() {
        let r = make_renderer(&[(ToolKind::Read, "read_file")], &[]);
        assert_eq!(r.render("${{ tools.by_kind.read }}").unwrap(), "read_file");
    }

    #[test]
    fn render_tool_name_with_override() {
        let r = make_renderer(&[(ToolKind::Read, "Read")], &[]);
        assert_eq!(r.render("${{ tools.by_kind.read }}").unwrap(), "Read");
    }

    #[test]
    fn render_param_name() {
        let r = make_renderer(
            &[(ToolKind::Edit, "Edit")],
            &[(ToolKind::Edit, &[("old_string", "find")])],
        );
        assert_eq!(r.render("${{ params.edit.old_string }}").unwrap(), "find");
    }

    /// `param_for_kind` is presence-aware at the *parameter* level, not just the
    /// tool level: it returns the renamed name, the identity name, or `None` —
    /// but never fabricates a name for a field the tool doesn't have.
    #[test]
    fn param_for_kind_presence_aware() {
        // Renamed param → client-facing name.
        let renamed = make_renderer(
            &[(ToolKind::Edit, "Edit")],
            &[(ToolKind::Edit, &[("old_string", "find")])],
        );
        assert_eq!(
            renamed.param_for_kind(ToolKind::Edit, "old_string"),
            Some("find")
        );

        // Present but unrenamed (seeded identity mapping) → canonical name.
        let identity = make_renderer(
            &[(ToolKind::Execute, "run_terminal_command")],
            &[(ToolKind::Execute, &[("is_background", "is_background")])],
        );
        assert_eq!(
            identity.param_for_kind(ToolKind::Execute, "is_background"),
            Some("is_background")
        );

        // Regression: an execute tool WITHOUT `is_background` in its schema
        // (e.g. OpenCode's foreground-only bash) seeds `params.execute` from its
        // real fields only. `param_for_kind` must report the missing field as
        // absent so `get_task_output` / `wait_tasks` don't tell the model to
        // pass `is_background=true` to a tool that has no such field.
        let opencode_bash = make_renderer(
            &[(ToolKind::Execute, "bash")],
            &[(
                ToolKind::Execute,
                &[
                    ("command", "command"),
                    ("timeout", "timeout"),
                    ("workdir", "workdir"),
                    ("description", "description"),
                ],
            )],
        );
        assert_eq!(
            opencode_bash.param_for_kind(ToolKind::Execute, "is_background"),
            None
        );

        // No tool of that kind at all → None.
        let empty = make_renderer(&[], &[]);
        assert_eq!(
            empty.param_for_kind(ToolKind::Execute, "is_background"),
            None
        );
    }

    #[test]
    fn render_schema_descriptions_resolves_param_refs() {
        let r = make_renderer(
            &[(ToolKind::Edit, "Edit")],
            &[(ToolKind::Edit, &[("old_string", "find")])],
        );
        let mut schema = serde_json::json!({
            "properties": {
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with (must be different from ${{ params.edit.old_string }})",
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences of ${{ params.edit.old_string }} (default false)",
                },
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to modify.",
                },
            }
        });
        r.render_schema_descriptions(&mut schema);
        assert_eq!(
            schema["properties"]["new_string"]["description"],
            "The text to replace it with (must be different from find)"
        );
        assert_eq!(
            schema["properties"]["replace_all"]["description"],
            "Replace all occurrences of find (default false)"
        );
        // Untemplated descriptions are left untouched.
        assert_eq!(
            schema["properties"]["file_path"]["description"],
            "The path to the file to modify."
        );
    }

    #[test]
    fn render_schema_descriptions_no_properties_is_noop() {
        let r = make_renderer(&[(ToolKind::Edit, "Edit")], &[]);
        let mut schema = serde_json::json!({"type": "object"});
        r.render_schema_descriptions(&mut schema);
        assert_eq!(schema, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn render_schema_descriptions_recurses_into_nested() {
        let r = make_renderer(
            &[(ToolKind::Edit, "Edit")],
            &[(ToolKind::Edit, &[("old_string", "find")])],
        );
        let mut schema = serde_json::json!({
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "note": {
                                "type": "string",
                                "description": "compare against ${{ params.edit.old_string }}",
                            }
                        }
                    }
                }
            }
        });
        r.render_schema_descriptions(&mut schema);
        assert_eq!(
            schema["properties"]["items"]["items"]["properties"]["note"]["description"],
            "compare against find"
        );
    }

    #[test]
    fn render_full_sentence() {
        let r = make_renderer(&[(ToolKind::Read, "Read"), (ToolKind::Search, "Grep")], &[]);
        let result = r
            .render("Use ${{ tools.by_kind.read }} to read. Or use ${{ tools.by_kind.search }}.")
            .unwrap();
        assert_eq!(result, "Use Read to read. Or use Grep.");
    }

    #[test]
    fn render_conditional_present() {
        let r = make_renderer(&[(ToolKind::Search, "Grep")], &[]);
        let result = r
            .render("${%- if tools.by_kind.search %}Use ${{ tools.by_kind.search }}.${%- endif %}")
            .unwrap();
        assert_eq!(result, "Use Grep.");
    }

    #[test]
    fn render_conditional_absent() {
        let r = make_renderer(&[], &[]);
        let result = r
            .render("${%- if tools.by_kind.search %}Use search.${%- endif %}OK")
            .unwrap();
        assert_eq!(result, "OK");
    }

    /// Documents MiniJinja's default (lenient) undefined behavior: a missing
    /// kind in *output* position renders as an empty string with `Ok`, it
    /// does NOT return `Err`. Callers that want a fallback name for a
    /// missing kind must check for an empty result — `.unwrap_or_else` on
    /// the `Result` alone never fires for this case.
    #[test]
    fn render_missing_kind_output_position_is_empty_ok() {
        let r = make_renderer(&[], &[]);
        let result = r.render("${{ tools.by_kind.background_task_action }}");
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn render_no_markers_fast_path() {
        let r = make_renderer(&[], &[]);
        let plain = "No template markers here.";
        assert_eq!(r.render(plain).unwrap(), plain);
    }

    #[test]
    fn render_invalid_template_returns_error() {
        let r = make_renderer(&[], &[]);
        let result = r.render("${%- if %}broken");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.template().contains("broken"));
    }

    #[test]
    fn render_multiple_kinds() {
        let r = make_renderer(
            &[
                (ToolKind::Read, "ReadFile"),
                (ToolKind::Edit, "Edit"),
                (ToolKind::Search, "Grep"),
                (ToolKind::Execute, "Bash"),
            ],
            &[],
        );
        let result = r
            .render(
                "${{ tools.by_kind.read }}, ${{ tools.by_kind.edit }}, ${{ tools.by_kind.search }}, ${{ tools.by_kind.execute }}",
            )
            .unwrap();
        assert_eq!(result, "ReadFile, Edit, Grep, Bash");
    }

    #[test]
    fn render_background_task_action_kind() {
        let r = make_renderer(&[(ToolKind::BackgroundTaskAction, "get_task_output")], &[]);
        let result = r
            .render("${{ tools.by_kind.background_task_action }}")
            .unwrap();
        assert_eq!(result, "get_task_output");
    }

    #[test]
    fn render_task_output_param_templates_not_raw_source() {
        let r = make_renderer(
            &[
                (ToolKind::Execute, "run_terminal_command"),
                (ToolKind::Task, "spawn_subagent"),
            ],
            &[
                (ToolKind::Execute, &[("is_background", "background")]),
                (ToolKind::Task, &[("run_in_background", "background")]),
            ],
        );
        let desc = r#"Get output and status from a background task or subagent.

Usage notes:
- Use the task_id from a command run with ${{ params.execute.is_background }}=true, or a subagent launched with ${{ params.task.run_in_background }}=true
- Omit timeout_ms (or pass 0) for a non-blocking status poll; set a positive timeout_ms to wait up to that many milliseconds for completion (capped at ~10 min)."#;
        let rendered = r.render(desc).expect("task_output description must render");
        let _ = std::fs::write("/tmp/task_output_tool_description.txt", &rendered);
        assert!(
            !rendered.contains("${{"),
            "must not leak raw template source: {rendered}"
        );
        assert!(rendered.contains("background=true"));
        assert!(rendered.contains("Omit timeout_ms") || rendered.contains("positive timeout_ms"));
    }

    #[tokio::test]
    async fn resolve_returns_canonical_names() {
        let mut resources = crate::types::resources::Resources::new();
        resources.insert(make_renderer(
            &[
                (ToolKind::Read, "read_file"),
                (ToolKind::Execute, "run_terminal_cmd"),
            ],
            &[(ToolKind::Edit, &[("old_string", "old_string")])],
        ));
        let shared = resources.into_shared();

        assert_eq!(
            TemplateRenderer::resolve(&shared, "${{ tools.by_kind.read }}")
                .await
                .unwrap(),
            "read_file"
        );
        assert_eq!(
            TemplateRenderer::resolve(&shared, "${{ tools.by_kind.execute }}")
                .await
                .unwrap(),
            "run_terminal_cmd"
        );
        assert_eq!(
            TemplateRenderer::resolve(&shared, "${{ params.edit.old_string }}")
                .await
                .unwrap(),
            "old_string"
        );
    }

    #[tokio::test]
    async fn resolve_returns_randomized_names() {
        let mut resources = crate::types::resources::Resources::new();
        resources.insert(make_renderer(
            &[
                (ToolKind::Read, "file_reader"),
                (ToolKind::Execute, "shell"),
            ],
            &[(
                ToolKind::Edit,
                &[("old_string", "find"), ("replace_all", "replaceAll")],
            )],
        ));
        let shared = resources.into_shared();

        assert_eq!(
            TemplateRenderer::resolve(&shared, "${{ tools.by_kind.read }}")
                .await
                .unwrap(),
            "file_reader"
        );
        assert_eq!(
            TemplateRenderer::resolve(&shared, "${{ tools.by_kind.execute }}")
                .await
                .unwrap(),
            "shell"
        );
        assert_eq!(
            TemplateRenderer::resolve(&shared, "${{ params.edit.old_string }}")
                .await
                .unwrap(),
            "find"
        );
        assert_eq!(
            TemplateRenderer::resolve(&shared, "${{ params.edit.replace_all }}")
                .await
                .unwrap(),
            "replaceAll"
        );
    }

    #[tokio::test]
    async fn resolve_errors_when_renderer_missing() {
        let resources = crate::types::resources::Resources::new();
        let shared = resources.into_shared();

        let result = TemplateRenderer::resolve(&shared, "${{ tools.by_kind.read }}").await;
        assert!(result.is_err());
    }

    #[test]
    fn conditional_sections_omit_absent_tools() {
        let r = make_renderer(
            &[(ToolKind::Read, "read_file"), (ToolKind::Search, "grep")],
            &[],
        );
        let template = "\
${%- if tools.by_kind.list or tools.by_kind.search or tools.by_kind.read or tools.by_kind.edit %}
Avoid using find/grep/cat. Instead use:${%- if tools.by_kind.list %}
- File search: Use ${{ tools.by_kind.list }}${%- endif %}${%- if tools.by_kind.search %}
- Content search: Use ${{ tools.by_kind.search }}${%- endif %}${%- if tools.by_kind.read %}
- Read files: Use ${{ tools.by_kind.read }}${%- endif %}${%- if tools.by_kind.edit %}
- Edit files: Use ${{ tools.by_kind.edit }}${%- endif %}${%- endif %}";

        let result = r.render(template).unwrap();
        assert!(result.contains("Content search: Use grep"));
        assert!(result.contains("Read files: Use read_file"));
        assert!(!result.contains("File search"));
        assert!(!result.contains("Edit files"));
    }

    #[test]
    fn conditional_sections_omit_entire_block_when_no_tools() {
        let r = make_renderer(&[], &[]);
        let template = "Before.\
${%- if tools.by_kind.list or tools.by_kind.search or tools.by_kind.read or tools.by_kind.edit %}
Avoid using find/grep/cat.${%- endif %}
After.";

        let result = r.render(template).unwrap();
        assert!(!result.contains("Avoid"));
        assert!(result.contains("Before."));
        assert!(result.contains("After."));
    }

    #[test]
    fn render_with_extra_bool_conditional() {
        let r = make_renderer(&[], &[]);
        let template = "before${%- if flag %} FLAG_ON${%- else %} FLAG_OFF${%- endif %} after";
        let on = r
            .render_with_extra(template, &serde_json::json!({"flag": true}))
            .unwrap();
        assert_eq!(on, "before FLAG_ON after");
        let off = r
            .render_with_extra(template, &serde_json::json!({"flag": false}))
            .unwrap();
        assert_eq!(off, "before FLAG_OFF after");
    }

    #[test]
    fn debug_shows_counts() {
        let r = make_renderer(
            &[(ToolKind::Read, "Read"), (ToolKind::Edit, "Edit")],
            &[(ToolKind::Edit, &[("old_string", "find")])],
        );
        let debug = format!("{:?}", r);
        assert!(debug.contains("tools: 2"));
        assert!(debug.contains("params: 1"));
    }
}
