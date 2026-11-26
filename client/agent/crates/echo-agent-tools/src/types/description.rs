//! MiniJinja-based description template rendering for tool descriptions.
//!
//! Tool descriptions often reference other tool names (e.g., "Use `read_file`
//! before editing"). When tool names are randomized for the model, these
//! references must be updated to use the model-facing names.
//!
//! This module provides:
//! - `make_desc_env()` — MiniJinja environment with custom `${{ }}`/`${%  %}`
//!   delimiters to avoid collisions with literal `{{ }}` in descriptions.
//! - `DescriptionContext` — maps tool/param names for template resolution.
//! - `resolve_description()` — renders a template with tool/param names,
//!   handling disabled tools via conditional sections.

use std::collections::HashMap;

/// Context for resolving tool description templates.
///
/// - `tools`: canonical name → `Some(model_facing_name)` if enabled, `None` if disabled.
/// - `params`: canonical tool name → { canonical param → model_facing param }.
/// - `skills`: available skills for the skill tool description.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DescriptionContext {
    /// Canonical tool name → `Some(model_facing_name)` (enabled) or `None` (disabled).
    pub tools: HashMap<String, Option<String>>,
    /// Canonical tool name → { canonical param → model_facing param }.
    pub params: HashMap<String, HashMap<String, String>>,
}

/// Create a MiniJinja environment with custom delimiters.
///
/// Delimiters:
/// - Variables: `${{ }}` (e.g., `${{ tools.read_file }}`)
/// - Blocks: `${%  %}` (e.g., `${%- if tools.grep %}...${%- endif %}`)
/// - Comments: `${#  #}` (e.g., `${# explanation #}`)
///
/// This avoids collisions with literal `{{ }}` that appear frequently in
/// tool descriptions (e.g., JSON examples, Rust generics).
pub fn make_desc_env() -> minijinja::Environment<'static> {
    use minijinja::syntax::SyntaxConfig;

    let syntax = SyntaxConfig::builder()
        .block_delimiters("${%", "%}")
        .variable_delimiters("${{", "}}")
        .comment_delimiters("${#", "#}")
        .build()
        .expect("custom syntax config is valid");

    let mut env = minijinja::Environment::new();
    env.set_syntax(syntax);
    env
}

/// Render a tool description template with tool/param name resolution.
///
/// # Arguments
///
/// - `template` — the description template string (may contain `${{ tools.X }}`
///   variables and `${%- if tools.X %}` conditionals).
/// - `context` — the `DescriptionContext` with tool/param name mappings.
///
/// # Returns
///
/// The rendered description string. On render failure (syntax error in template),
/// falls back to returning the raw template unchanged — this ensures tool
/// registration never fails due to a template issue.
///
/// # Examples
///
/// ```ignore
/// let mut ctx = DescriptionContext::default();
/// ctx.tools.insert("read_file".into(), Some("Read".into()));
/// ctx.tools.insert("grep".into(), None); // disabled
///
/// let desc = resolve_description(
///     "Use ${{ tools.read_file }} to read. ${%- if tools.grep %} Also use ${{ tools.grep }}.${%- endif %}",
///     &ctx,
/// );
/// assert_eq!(desc, "Use Read to read.");
/// ```
pub fn resolve_description(template: &str, context: &DescriptionContext) -> String {
    // Fast path: if the template doesn't contain any MiniJinja delimiters,
    // skip the render entirely.
    if !template.contains("${{") && !template.contains("${%") {
        return template.to_string();
    }

    let mut env = make_desc_env();
    env.add_template("desc", template)
        .and_then(|()| {
            let tmpl = env.get_template("desc")?;
            tmpl.render(context)
        })
        .unwrap_or_else(|e| {
            tracing::warn!("Description template render failed, using raw template: {e}");
            template.to_string()
        })
}

/// Render a Task description template that loops over `model_slugs`.
///
/// Sorts and deduplicates slugs, then renders with the standard `${{ }}` /
/// `${% %}` description delimiters. Used when a harness embeds a sorted
/// model-catalog list in a Task description template.
pub fn render_with_model_slugs(template: &str, model_slugs: &[String]) -> String {
    let mut model_slugs = model_slugs.to_vec();
    model_slugs.sort_unstable();
    model_slugs.dedup();

    let env = make_desc_env();
    env.render_str(template, minijinja::context! { model_slugs => model_slugs })
        .expect("Task description template with model_slugs must render")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_tools(tools: &[(&str, Option<&str>)]) -> DescriptionContext {
        let mut ctx = DescriptionContext::default();
        for (name, model_name) in tools {
            ctx.tools
                .insert(name.to_string(), model_name.map(|s| s.to_string()));
        }
        ctx
    }

    #[test]
    fn variable_substitution() {
        let ctx = ctx_with_tools(&[("read_file", Some("Read"))]);
        let result = resolve_description("Use ${{ tools.read_file }} to read files.", &ctx);
        assert_eq!(result, "Use Read to read files.");
    }

    #[test]
    fn conditional_section_enabled() {
        let ctx = ctx_with_tools(&[("grep", Some("grep"))]);
        let result = resolve_description("${%- if tools.grep %}Use grep.${%- endif %}", &ctx);
        assert_eq!(result, "Use grep.");
    }

    #[test]
    fn conditional_section_disabled() {
        let ctx = ctx_with_tools(&[("grep", None)]);
        let result = resolve_description("${%- if tools.grep %}Use grep.${%- endif %}", &ctx);
        assert_eq!(result, "");
    }

    #[test]
    fn literal_braces_pass_through() {
        let ctx = ctx_with_tools(&[]);
        let result = resolve_description("Use {{ literal_braces }} and {another} in prose.", &ctx);
        assert_eq!(result, "Use {{ literal_braces }} and {another} in prose.");
    }

    #[test]
    fn fallback_to_raw_template_on_render_failure() {
        let ctx = ctx_with_tools(&[]);
        // This has invalid syntax: ${%- if without endif
        let template = "${%- if %}broken";
        let result = resolve_description(template, &ctx);
        // Should fall back to raw template
        assert_eq!(result, template);
    }

    #[test]
    fn no_template_markers_returns_unchanged() {
        let ctx = ctx_with_tools(&[("read_file", Some("Read"))]);
        let plain = "This description has no template markers.";
        let result = resolve_description(plain, &ctx);
        assert_eq!(result, plain);
    }

    #[test]
    fn render_with_model_slugs_sorts_dedups_and_loops() {
        let template = "\
${% if model_slugs %}
list:
${%- for slug in model_slugs %}
- ${{ slug }}
${%- endfor %}
${% else %}
empty
${% endif %}";
        let rendered = render_with_model_slugs(
            template,
            &["zeta".to_string(), "alpha".to_string(), "alpha".to_string()],
        );
        assert!(rendered.contains("- alpha"));
        assert!(rendered.contains("- zeta"));
        assert!(!rendered.contains("empty"));
        assert!(!rendered.contains("${{"));
        assert_eq!(render_with_model_slugs(template, &[]).trim(), "empty");
    }

    #[test]
    fn multiple_tools_in_one_description() {
        let ctx = ctx_with_tools(&[
            ("read_file", Some("Read")),
            ("grep", Some("Search")),
            ("search_replace", Some("Edit")),
        ]);
        let template = "Use ${{ tools.read_file }} first, then ${{ tools.grep }}, then ${{ tools.search_replace }}.";
        let result = resolve_description(template, &ctx);
        assert_eq!(result, "Use Read first, then Search, then Edit.");
    }

    #[test]
    fn param_name_substitution() {
        let mut ctx = ctx_with_tools(&[("search_replace", Some("edit"))]);
        let mut params = HashMap::new();
        params.insert("old_string".to_string(), "find".to_string());
        params.insert("new_string".to_string(), "replace_with".to_string());
        ctx.params.insert("search_replace".to_string(), params);

        let template =
            "The ${{ params.search_replace.old_string }} parameter specifies what to find.";
        let result = resolve_description(template, &ctx);
        assert_eq!(result, "The find parameter specifies what to find.");
    }

    #[test]
    fn mixed_conditionals_and_substitutions() {
        let ctx = ctx_with_tools(&[
            ("read_file", Some("Read")),
            ("grep", None), // disabled
        ]);
        let template = "Always use ${{ tools.read_file }}.${%- if tools.grep %} Also use ${{ tools.grep }}.${%- endif %}";
        let result = resolve_description(template, &ctx);
        assert_eq!(result, "Always use Read.");
    }

    #[test]
    fn empty_template_returns_empty() {
        let ctx = ctx_with_tools(&[]);
        assert_eq!(resolve_description("", &ctx), "");
    }

    #[test]
    fn canonical_names_as_default_when_no_override() {
        let ctx = ctx_with_tools(&[("read_file", Some("read_file"))]);
        let template = "Use ${{ tools.read_file }} tool.";
        let result = resolve_description(template, &ctx);
        assert_eq!(result, "Use read_file tool.");
    }
}
