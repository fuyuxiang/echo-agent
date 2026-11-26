use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    #[default]
    Build,
    Chat,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKind::Build => "build",
            SessionKind::Chat => "chat",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FacetValue {
    One(serde_json::Value),
    Many(Vec<serde_json::Value>),
}

impl FacetValue {
    pub fn values(&self) -> Vec<&serde_json::Value> {
        match self {
            FacetValue::One(v) => vec![v],
            FacetValue::Many(vs) => vs.iter().collect(),
        }
    }

    pub fn intersects(&self, allowed: &[serde_json::Value]) -> bool {
        self.values().into_iter().any(|v| allowed.contains(v))
    }
}

pub type FacetMap = BTreeMap<String, FacetValue>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetaEnvelope {
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "FacetMap::is_empty")]
    pub facets: FacetMap,
}
