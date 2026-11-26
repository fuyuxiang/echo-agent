//! Model-facing listing of discovered workflows.
//!
//! Rendered under the skill catalog in the baseline `<system-reminder>` so
//! the model can launch a saved workflow by name the same way it sees skills.

use super::registry::WorkflowListing;
use echo_agent_tools::util::truncate_str_with_marker;

/// Per-entry cap on description + when_to_use combined. The script body is
/// loaded on launch, so the listing stays terse.
const MAX_LISTING_COMBINED_BYTES: usize = 400;
const MIN_FIELD_BYTES: usize = 20;

pub(crate) const WORKFLOW_LISTING_HEADER: &str = "The following workflows are available:\n\n";

/// Render the discovered workflow catalog, or `None` when there are none.
pub(crate) fn format_workflow_listing(workflows: &[WorkflowListing]) -> Option<String> {
    if workflows.is_empty() {
        return None;
    }
    let mut body = String::from(WORKFLOW_LISTING_HEADER);
    for (i, workflow) in workflows.iter().enumerate() {
        if i > 0 {
            body.push('\n');
        }
        body.push_str(&format_entry(workflow));
    }
    Some(body)
}

/// Concatenate the skill listing and the workflow listing for one reminder.
pub(crate) fn merge_listing_sections(
    skills: Option<&str>,
    workflows: Option<&str>,
) -> Option<String> {
    match (
        skills.filter(|text| !text.is_empty()),
        workflows.filter(|text| !text.is_empty()),
    ) {
        (Some(skills), Some(workflows)) => Some(format!("{skills}\n\n{workflows}")),
        (Some(skills), None) => Some(skills.to_string()),
        (None, Some(workflows)) => Some(workflows.to_string()),
        (None, None) => None,
    }
}

fn format_entry(workflow: &WorkflowListing) -> String {
    let (desc_budget, wtu_budget) = field_budgets(workflow);
    let desc = truncate_str_with_marker(&workflow.description, desc_budget);
    let mut out = format!("- {}: {desc}", workflow.name);
    if let Some(when) = workflow
        .when_to_use
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        let when = truncate_str_with_marker(when, wtu_budget);
        out.push_str(&format!("\n  Use when: {when}"));
    }
    if let Some(path) = workflow.path.as_deref().filter(|text| !text.is_empty()) {
        out.push_str(&format!("\n  Absolute path: {path}"));
    }
    out
}

fn field_budgets(workflow: &WorkflowListing) -> (usize, usize) {
    let Some(when) = workflow
        .when_to_use
        .as_deref()
        .filter(|text| !text.is_empty())
    else {
        return (MAX_LISTING_COMBINED_BYTES, 0);
    };
    let desc_len = workflow.description.len().max(1);
    let when_len = when.len().max(1);
    let combined = desc_len + when_len;
    let desc_budget = MAX_LISTING_COMBINED_BYTES * desc_len / combined;
    let wtu_budget = MAX_LISTING_COMBINED_BYTES.saturating_sub(desc_budget);
    if desc_budget < MIN_FIELD_BYTES && wtu_budget > MIN_FIELD_BYTES {
        (
            MIN_FIELD_BYTES,
            MAX_LISTING_COMBINED_BYTES.saturating_sub(MIN_FIELD_BYTES),
        )
    } else if wtu_budget < MIN_FIELD_BYTES && desc_budget > MIN_FIELD_BYTES {
        (
            MAX_LISTING_COMBINED_BYTES.saturating_sub(MIN_FIELD_BYTES),
            MIN_FIELD_BYTES,
        )
    } else {
        (desc_budget, wtu_budget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(
        name: &str,
        description: &str,
        when_to_use: Option<&str>,
        source: &'static str,
        path: Option<&str>,
    ) -> WorkflowListing {
        WorkflowListing {
            name: name.to_string(),
            description: description.to_string(),
            when_to_use: when_to_use.map(str::to_string),
            source,
            path: path.map(str::to_string),
        }
    }

    #[test]
    fn empty_catalog_is_none() {
        assert!(format_workflow_listing(&[]).is_none());
    }

    #[test]
    fn builtin_includes_when_to_use_and_path() {
        let text = format_workflow_listing(&[listing(
            "deep-research",
            "Research a query with citations.",
            Some("Compare or research a question that needs sourced claims"),
            "builtin",
            Some("/src/session/workflows/deep_research.rhai"),
        )])
        .unwrap();
        assert!(text.starts_with(WORKFLOW_LISTING_HEADER), "got:\n{text}");
        assert!(text.contains("- deep-research: Research a query with citations."));
        assert!(
            text.contains("  Use when: Compare or research a question that needs sourced claims")
        );
        assert!(!text.contains("Source:"));
        assert!(text.contains("  Absolute path: /src/session/workflows/deep_research.rhai"));
    }

    #[test]
    fn file_backed_entry_includes_when_to_use_and_path() {
        let text = format_workflow_listing(&[listing(
            "review-pr",
            "Review a GitHub PR and post findings.",
            Some("Review a pull request"),
            "user",
            Some("/Users/dev/.echo-agent/workflows/review-pr.rhai"),
        )])
        .unwrap();
        assert!(text.contains("- review-pr: Review a GitHub PR and post findings."));
        assert!(text.contains("  Use when: Review a pull request"));
        assert!(!text.contains("Source:"));
        assert!(text.contains("  Absolute path: /Users/dev/.echo-agent/workflows/review-pr.rhai"));
    }

    #[test]
    fn merge_puts_workflows_under_skills() {
        let merged = merge_listing_sections(
            Some("The following skills are available for use:\n\n- commit: Make a commit."),
            Some("The following workflows are available:\n\n- review-pr: Review a PR."),
        )
        .unwrap();
        assert!(merged.contains("skills are available"));
        assert!(merged.contains("workflows are available"));
        assert!(
            merged.find("skills are available").unwrap()
                < merged.find("workflows are available").unwrap()
        );
    }

    #[test]
    fn merge_survives_a_missing_side() {
        assert_eq!(
            merge_listing_sections(Some("skills"), None).as_deref(),
            Some("skills")
        );
        assert_eq!(
            merge_listing_sections(None, Some("workflows")).as_deref(),
            Some("workflows")
        );
        assert!(merge_listing_sections(None, None).is_none());
        assert!(merge_listing_sections(Some(""), Some("")).is_none());
    }
}
