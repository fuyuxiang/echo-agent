use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Shared bundle payload for subagent persona, role, and agent definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentBundle {
    pub version: String,
    pub personas: HashMap<String, String>,
    pub roles: HashMap<String, String>,
    pub agents: HashMap<String, String>,
    #[serde(default)]
    pub skills: HashMap<String, String>,
}

impl SubagentBundle {
    pub fn empty(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            personas: HashMap::new(),
            roles: HashMap::new(),
            agents: HashMap::new(),
            skills: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SubagentBundle;
    use std::collections::HashMap;

    #[test]
    fn serializes_expected_shape() {
        let bundle = SubagentBundle {
            version: "bundle-v1".to_owned(),
            personas: HashMap::from([("researcher".to_owned(), "persona body".to_owned())]),
            roles: HashMap::from([("reviewer".to_owned(), "role body".to_owned())]),
            agents: HashMap::from([("default".to_owned(), "agent body".to_owned())]),
            skills: HashMap::from([("commit".to_owned(), "skill body".to_owned())]),
        };

        let actual = serde_json::to_value(bundle).unwrap();
        let expected = serde_json::json!({
            "version": "bundle-v1",
            "personas": {
                "researcher": "persona body"
            },
            "roles": {
                "reviewer": "role body"
            },
            "agents": {
                "default": "agent body"
            },
            "skills": {
                "commit": "skill body"
            }
        });

        assert_eq!(expected, actual);
    }

    #[test]
    fn deserializes_without_skills_field() {
        let json = serde_json::json!({
            "version": "bundle-v1",
            "personas": {},
            "roles": {},
            "agents": {}
        });

        let bundle: SubagentBundle = serde_json::from_value(json).unwrap();
        assert_eq!(bundle.version, "bundle-v1");
        assert!(bundle.skills.is_empty());
        assert!(SubagentBundle::empty("v1").skills.is_empty());
    }

    #[test]
    fn round_trips_with_skills() {
        let bundle = SubagentBundle {
            version: "v2".to_owned(),
            personas: HashMap::new(),
            roles: HashMap::new(),
            agents: HashMap::new(),
            skills: HashMap::from([
                (
                    "commit".to_owned(),
                    "---\nname: commit\n---\n# Commit".to_owned(),
                ),
                (
                    "review".to_owned(),
                    "---\nname: review\n---\n# Review".to_owned(),
                ),
            ]),
        };

        let json = serde_json::to_string(&bundle).unwrap();
        let deserialized: SubagentBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(bundle, deserialized);
    }
}
