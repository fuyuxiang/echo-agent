#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MermaidTheme {
    pub background: String,
    pub node_fill: String,
    pub node_stroke: String,
    pub text_color: String,
    pub edge_color: String,
    pub subgraph_fill: String,
    pub subgraph_stroke: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MermaidThemePreset {
    Default,
    Base,
    Dark,
    Forest,
    Neutral,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct MermaidThemeVariables {
    pub background: Option<String>,
    pub node_fill: Option<String>,
    pub node_stroke: Option<String>,
    pub text_color: Option<String>,
    pub edge_color: Option<String>,
    pub subgraph_fill: Option<String>,
    pub subgraph_stroke: Option<String>,
}

impl Default for MermaidTheme {
    fn default() -> Self {
        Self::light()
    }
}

impl MermaidThemePreset {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "base" => Some(Self::Base),
            "dark" => Some(Self::Dark),
            "forest" => Some(Self::Forest),
            "neutral" => Some(Self::Neutral),
            _ => None,
        }
    }

    pub fn to_theme(self) -> MermaidTheme {
        match self {
            Self::Default => MermaidTheme::light(),
            Self::Base => MermaidTheme::base(),
            Self::Dark => MermaidTheme::dark(),
            Self::Forest => MermaidTheme::forest(),
            Self::Neutral => MermaidTheme::neutral(),
        }
    }
}

impl MermaidThemeVariables {
    pub fn is_empty(&self) -> bool {
        self.background.is_none()
            && self.node_fill.is_none()
            && self.node_stroke.is_none()
            && self.text_color.is_none()
            && self.edge_color.is_none()
            && self.subgraph_fill.is_none()
            && self.subgraph_stroke.is_none()
    }

    pub fn apply_mermaid_alias(&mut self, key: &str, value: String) -> bool {
        match key {
            "background" => self.background = Some(value),
            "primaryColor" | "mainBkg" => self.node_fill = Some(value),
            "primaryBorderColor" | "nodeBorder" => self.node_stroke = Some(value),
            "primaryTextColor" | "nodeTextColor" | "textColor" => self.text_color = Some(value),
            "lineColor" | "defaultLinkColor" => self.edge_color = Some(value),
            "clusterBkg" => self.subgraph_fill = Some(value),
            "clusterBorder" => self.subgraph_stroke = Some(value),
            _ => return false,
        }

        true
    }

    pub fn apply_to(&self, theme: &mut MermaidTheme) {
        if let Some(value) = &self.background {
            theme.background.clone_from(value);
        }
        if let Some(value) = &self.node_fill {
            theme.node_fill.clone_from(value);
        }
        if let Some(value) = &self.node_stroke {
            theme.node_stroke.clone_from(value);
        }
        if let Some(value) = &self.text_color {
            theme.text_color.clone_from(value);
        }
        if let Some(value) = &self.edge_color {
            theme.edge_color.clone_from(value);
        }
        if let Some(value) = &self.subgraph_fill {
            theme.subgraph_fill.clone_from(value);
        }
        if let Some(value) = &self.subgraph_stroke {
            theme.subgraph_stroke.clone_from(value);
        }
    }
}

impl MermaidTheme {
    pub fn light() -> Self {
        Self {
            background: "#ffffff".to_string(),
            node_fill: "#ECECFF".to_string(),
            node_stroke: "#9370DB".to_string(),
            text_color: "#333333".to_string(),
            edge_color: "#333333".to_string(),
            subgraph_fill: "#ffffde".to_string(),
            subgraph_stroke: "#aaaa33".to_string(),
        }
    }

    pub fn dark() -> Self {
        Self {
            background: "#1e1e1e".to_string(),
            node_fill: "#2d2d2d".to_string(),
            node_stroke: "#888888".to_string(),
            text_color: "#ffffff".to_string(),
            edge_color: "#888888".to_string(),
            subgraph_fill: "#3a3a20".to_string(),
            subgraph_stroke: "#888844".to_string(),
        }
    }

    pub fn base() -> Self {
        Self::light()
    }

    pub fn forest() -> Self {
        Self {
            background: "#f4f4f4".to_string(),
            node_fill: "#cde498".to_string(),
            node_stroke: "#13540c".to_string(),
            text_color: "#333333".to_string(),
            edge_color: "#333333".to_string(),
            subgraph_fill: "#cde498".to_string(),
            subgraph_stroke: "#13540c".to_string(),
        }
    }

    pub fn neutral() -> Self {
        Self {
            background: "#ffffff".to_string(),
            node_fill: "#eeeeee".to_string(),
            node_stroke: "#999999".to_string(),
            text_color: "#333333".to_string(),
            edge_color: "#333333".to_string(),
            subgraph_fill: "#eeeeee".to_string(),
            subgraph_stroke: "#999999".to_string(),
        }
    }
}
