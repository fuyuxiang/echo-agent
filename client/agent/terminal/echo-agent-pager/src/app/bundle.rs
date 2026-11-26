//! Bundle status state and response types.
//!
//! Pager-side cache of what `echo-agent-runtime` reports from
//! `echo.agent/bundle/status`. The shell now performs the actual bundle download in
//! the background post-auth; the pager only reads the resulting on-disk
//! catalog so it can populate the welcome-screen subagent pane.

use serde::Deserialize;

/// Pager-local snapshot of bundle availability on disk.
///
/// Populated from `echo.agent/bundle/status` ACP responses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BundleState {
    pub has_cache: bool,
    pub version: String,
    pub personas: Vec<String>,
    pub roles: Vec<String>,
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    pub persona_details: Vec<PersonaDetail>,
    pub role_details: Vec<RoleDetail>,
}

/// Deserialized response from `echo.agent/bundle/status`.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BundleStatusResult {
    pub has_cache: bool,
    /// `None` when `has_cache` is false (shell omits the field).
    pub version: Option<String>,
    pub personas: Vec<String>,
    pub roles: Vec<String>,
    pub agents: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub persona_details: Vec<PersonaDetail>,
    #[serde(default)]
    pub role_details: Vec<RoleDetail>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersonaDetail {
    pub name: String,
    pub description: Option<String>,
    pub has_inputs: bool,
    pub has_outputs: bool,
    /// Absolute path when the persona was loaded from disk (user/project).
    #[serde(default)]
    pub source_path: Option<String>,
    /// `"user"` or `"project"` for local personas; omitted for bundled catalog entries.
    #[serde(default)]
    pub scope_label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoleDetail {
    pub name: String,
    pub description: String,
}

/// Deserialized response from `echo.agent/bundle/entry/get`.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EntryGetResult {
    pub kind: String,
    pub name: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_status_result_without_details() {
        let json = r#"{
            "hasCache": true,
            "version": "v1",
            "personas": ["researcher", "auditor"],
            "roles": ["reviewer"],
            "agents": ["default", "plan"]
        }"#;
        let r: BundleStatusResult = serde_json::from_str(json).expect("parse");
        assert!(r.has_cache);
        assert_eq!(r.version.as_deref(), Some("v1"));
        assert_eq!(r.personas, vec!["researcher", "auditor"]);
        assert_eq!(r.roles, vec!["reviewer"]);
        assert_eq!(r.agents, vec!["default", "plan"]);
        assert!(r.persona_details.is_empty());
        assert!(r.role_details.is_empty());
        assert!(r.skills.is_empty());
    }

    #[test]
    fn deserialize_status_result_with_details() {
        let json = r#"{
            "hasCache": true,
            "version": "v2",
            "personas": ["researcher"],
            "roles": ["reviewer"],
            "agents": [],
            "skills": ["commit", "design"],
            "personaDetails": [{
                "name": "researcher",
                "description": "thorough researcher",
                "hasInputs": true,
                "hasOutputs": false
            }],
            "roleDetails": [{
                "name": "reviewer",
                "description": "code reviewer"
            }]
        }"#;
        let r: BundleStatusResult = serde_json::from_str(json).expect("parse");
        assert_eq!(r.persona_details.len(), 1);
        assert_eq!(r.persona_details[0].name, "researcher");
        assert_eq!(
            r.persona_details[0].description.as_deref(),
            Some("thorough researcher")
        );
        assert!(r.persona_details[0].has_inputs);
        assert!(!r.persona_details[0].has_outputs);
        assert_eq!(r.role_details.len(), 1);
        assert_eq!(r.role_details[0].name, "reviewer");
        assert_eq!(r.role_details[0].description, "code reviewer");
        assert_eq!(r.skills, vec!["commit", "design"]);
    }

    #[test]
    fn deserialize_status_result_empty() {
        let json = r#"{
            "hasCache": false,
            "version": "",
            "personas": [],
            "roles": [],
            "agents": []
        }"#;
        let r: BundleStatusResult = serde_json::from_str(json).expect("parse");
        assert!(!r.has_cache);
        assert!(r.personas.is_empty());
        assert!(r.skills.is_empty());
    }

    #[test]
    fn deserialize_status_result_no_version_field() {
        // The shell omits `version` entirely when has_cache is false.
        let json = r#"{
            "hasCache": false,
            "personas": [],
            "roles": [],
            "agents": []
        }"#;
        let r: BundleStatusResult = serde_json::from_str(json).expect("parse");
        assert!(!r.has_cache);
        assert!(r.version.is_none());
    }

    #[test]
    fn deserialize_entry_get_result() {
        let json = r#"{
            "kind": "persona",
            "name": "researcher",
            "content": "instructions = \"dig deep\""
        }"#;
        let r: EntryGetResult = serde_json::from_str(json).expect("parse");
        assert_eq!(r.kind, "persona");
        assert_eq!(r.name, "researcher");
        assert_eq!(r.content, "instructions = \"dig deep\"");
    }
}
