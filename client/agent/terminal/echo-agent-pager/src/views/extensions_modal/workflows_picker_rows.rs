//! Picker rows for the extensions modal's Workflows tab.

use super::{TabDataState, WorkflowInfo, cmp_str_ci, fuzzy_matches};

/// Placeholder row when the catalog comes back empty (also what a disabled
/// workflows feature looks like on the wire, hence the hedged phrasing).
pub(super) const WORKFLOWS_EMPTY_PLACEHOLDER: &str =
    "No workflows available. Ask EchoAgent to help make you one!";

/// One picker row for the Workflows tab (flat, browse-only catalog).
#[derive(Debug)]
pub(super) struct WorkflowRow {
    pub(super) label: String,
    pub(super) right_label: String,
    pub(super) desc_lines: Vec<String>,
    pub(super) fields: Vec<(String, String)>,
    pub(super) dimmed: bool,
}

impl WorkflowRow {
    /// Dimmed single-label row (empty catalog or fetch error).
    fn notice(label: String) -> Self {
        Self {
            label,
            right_label: String::new(),
            desc_lines: Vec::new(),
            fields: Vec::new(),
            dimmed: true,
        }
    }
}

/// Build the Workflows-tab rows, A–Z by name, fuzzy-filtered on
/// name+description like the Hooks/Plugins tabs. An empty catalog yields a
/// single dimmed placeholder row; an error yields a dimmed error row.
pub(super) fn build_workflows_picker_rows(
    data: &TabDataState<Vec<WorkflowInfo>>,
    query: &str,
) -> Vec<WorkflowRow> {
    let workflows = match data {
        TabDataState::Loaded(workflows) => workflows,
        TabDataState::Error(msg) => return vec![WorkflowRow::notice(format!("Error: {msg}"))],
        // The render gate skips entry building while loading.
        TabDataState::Loading => return Vec::new(),
    };
    if workflows.is_empty() {
        return vec![WorkflowRow::notice(WORKFLOWS_EMPTY_PLACEHOLDER.to_string())];
    }
    let mut visible: Vec<&WorkflowInfo> = workflows
        .iter()
        .filter(|w| fuzzy_matches(&w.name, query) || fuzzy_matches(&w.description, query))
        .collect();
    visible.sort_by(|a, b| cmp_str_ci(&a.name, &b.name));
    visible
        .into_iter()
        .map(|wf| {
            let mut fields = Vec::new();
            if let Some(ref p) = wf.path {
                fields.push(("path".to_string(), p.clone()));
            }
            if let Some(ref w) = wf.when_to_use {
                fields.push(("when to use".to_string(), w.clone()));
            }
            WorkflowRow {
                label: wf.name.clone(),
                right_label: format!("({})", wf.source),
                desc_lines: if wf.description.is_empty() {
                    Vec::new()
                } else {
                    vec![wf.description.clone()]
                },
                fields,
                dimmed: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(rows: &[WorkflowRow]) -> Vec<&str> {
        rows.iter().map(|row| row.label.as_str()).collect()
    }

    #[test]
    fn filters_on_name_and_description() {
        let workflows = TabDataState::Loaded(vec![
            WorkflowInfo {
                name: "alpha-wf".into(),
                description: "touches ci".into(),
                when_to_use: None,
                source: "user".into(),
                path: Some("/home/u/.echo-agent/workflows/alpha-wf.rhai".into()),
            },
            WorkflowInfo {
                name: "beta-wf".into(),
                description: "docs".into(),
                when_to_use: None,
                source: "user".into(),
                path: None,
            },
        ]);
        let by_desc = build_workflows_picker_rows(&workflows, "ci");
        assert_eq!(labels(&by_desc), ["alpha-wf"]);
        assert_eq!(
            by_desc[0].fields,
            [(
                "path".to_string(),
                "/home/u/.echo-agent/workflows/alpha-wf.rhai".to_string()
            )]
        );
        let by_name = build_workflows_picker_rows(&workflows, "beta");
        assert_eq!(labels(&by_name), ["beta-wf"]);
        // Subsequence match, same as the Hooks/Plugins tabs.
        let by_subsequence = build_workflows_picker_rows(&workflows, "alphawf");
        assert_eq!(labels(&by_subsequence), ["alpha-wf"]);
        let none = build_workflows_picker_rows(&workflows, "zzz");
        assert!(
            none.is_empty(),
            "query misses yield no rows (picker shows its No matches state)"
        );
    }

    #[test]
    fn error_and_loading_states_build_their_own_rows() {
        let error = build_workflows_picker_rows(&TabDataState::Error("boom".into()), "");
        assert_eq!(labels(&error), ["Error: boom"]);
        assert!(error[0].dimmed);
        let loading = build_workflows_picker_rows(&TabDataState::Loading, "");
        assert!(loading.is_empty());
        let empty = build_workflows_picker_rows(&TabDataState::Loaded(vec![]), "");
        assert_eq!(labels(&empty), [WORKFLOWS_EMPTY_PLACEHOLDER]);
        assert!(empty[0].dimmed);
    }
}
