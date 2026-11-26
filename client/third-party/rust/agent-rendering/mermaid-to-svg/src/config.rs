use std::borrow::Cow;

use serde_yaml::Value;

use crate::theme::{MermaidTheme, MermaidThemePreset, MermaidThemeVariables};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedMermaidSource<'a> {
    pub body: Cow<'a, str>,
    pub frontmatter: Option<MermaidFrontmatter>,
    pub config: RenderConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MermaidFrontmatter {
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderConfig {
    pub theme: Option<MermaidThemePreset>,
    pub theme_variables: MermaidThemeVariables,
    /// Parsed for Mermaid frontmatter compatibility, but not currently rendered.
    pub layout: Option<String>,
    /// Parsed for Mermaid frontmatter compatibility, but not currently rendered.
    pub look: Option<String>,
    /// Parsed for Mermaid frontmatter compatibility, but not currently rendered.
    pub security_level: Option<String>,
    pub font_family: Option<String>,
    pub font_size: Option<String>,
    pub flowchart: FlowchartConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowchartConfig {
    pub curve: Option<String>,
    /// Parsed for Mermaid frontmatter compatibility, but not currently rendered.
    pub html_labels: Option<bool>,
    pub node_spacing: Option<u32>,
    pub rank_spacing: Option<u32>,
    pub padding: Option<u32>,
    /// Parsed for Mermaid frontmatter compatibility, but not currently rendered.
    pub diagram_padding: Option<u32>,
    pub wrapping_width: Option<u32>,
    /// Parsed for Mermaid frontmatter compatibility, but not currently rendered.
    pub use_max_width: Option<bool>,
    /// Parsed for Mermaid frontmatter compatibility, but not currently rendered.
    pub default_renderer: Option<String>,
}

impl RenderConfig {
    pub fn to_mermaid_theme(&self) -> Option<MermaidTheme> {
        if self.theme.is_none() && self.theme_variables.is_empty() {
            return None;
        }

        let mut theme = self.theme.unwrap_or(MermaidThemePreset::Default).to_theme();
        self.theme_variables.apply_to(&mut theme);
        Some(theme)
    }

    pub fn font_size_px(&self) -> Option<f64> {
        self.font_size.as_deref().and_then(parse_font_size)
    }
}

pub fn parse_mermaid_frontmatter(source: &str) -> ParsedMermaidSource<'_> {
    let Some((yaml_start, yaml_end, body_start)) = frontmatter_bounds(source) else {
        return ParsedMermaidSource {
            body: Cow::Borrowed(source),
            frontmatter: None,
            config: RenderConfig::default(),
        };
    };

    let body = Cow::Owned(source[body_start..].to_string());
    let yaml = &source[yaml_start..yaml_end];
    let Some(value) = parse_yaml_value(yaml) else {
        return ParsedMermaidSource {
            body,
            frontmatter: Some(MermaidFrontmatter::default()),
            config: RenderConfig::default(),
        };
    };

    let frontmatter = parse_frontmatter_metadata(&value);
    let config = parse_render_config(&value);

    ParsedMermaidSource {
        body,
        frontmatter: Some(frontmatter),
        config,
    }
}

fn parse_yaml_value(yaml: &str) -> Option<Value> {
    if yaml.trim().is_empty() {
        return Some(Value::Null);
    }

    serde_yaml::from_str::<Value>(yaml).ok()
}

fn parse_frontmatter_metadata(value: &Value) -> MermaidFrontmatter {
    MermaidFrontmatter {
        title: mapping_value(value, "title").and_then(value_to_string),
    }
}

fn parse_render_config(value: &Value) -> RenderConfig {
    let Some(config) = mapping_value(value, "config") else {
        return RenderConfig::default();
    };

    RenderConfig {
        theme: mapping_value(config, "theme")
            .and_then(value_to_string)
            .and_then(|theme| MermaidThemePreset::parse(&theme)),
        theme_variables: parse_theme_variables(mapping_value(config, "themeVariables")),
        layout: mapping_value(config, "layout").and_then(value_to_string),
        look: mapping_value(config, "look").and_then(value_to_string),
        security_level: mapping_value(config, "securityLevel").and_then(value_to_string),
        font_family: mapping_value(config, "fontFamily").and_then(value_to_string),
        font_size: mapping_value(config, "fontSize").and_then(value_to_string),
        flowchart: parse_flowchart_config(mapping_value(config, "flowchart")),
    }
}

fn parse_theme_variables(value: Option<&Value>) -> MermaidThemeVariables {
    let mut variables = MermaidThemeVariables::default();
    let Some(Value::Mapping(mapping)) = value else {
        return variables;
    };

    for (key, value) in mapping {
        let Some(key) = key.as_str() else {
            continue;
        };
        let Some(value) = value_to_string(value) else {
            continue;
        };
        variables.apply_mermaid_alias(key, value);
    }

    variables
}

fn parse_flowchart_config(value: Option<&Value>) -> FlowchartConfig {
    let Some(flowchart) = value else {
        return FlowchartConfig::default();
    };

    FlowchartConfig {
        curve: mapping_value(flowchart, "curve").and_then(value_to_string),
        html_labels: mapping_value(flowchart, "htmlLabels").and_then(value_to_bool),
        node_spacing: mapping_value(flowchart, "nodeSpacing").and_then(value_to_u32),
        rank_spacing: mapping_value(flowchart, "rankSpacing").and_then(value_to_u32),
        padding: mapping_value(flowchart, "padding").and_then(value_to_u32),
        diagram_padding: mapping_value(flowchart, "diagramPadding").and_then(value_to_u32),
        wrapping_width: mapping_value(flowchart, "wrappingWidth").and_then(value_to_u32),
        use_max_width: mapping_value(flowchart, "useMaxWidth").and_then(value_to_bool),
        default_renderer: mapping_value(flowchart, "defaultRenderer").and_then(value_to_string),
    }
}

fn mapping_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    let Value::Mapping(mapping) = value else {
        return None;
    };

    mapping.get(&Value::String(key.to_string()))
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_to_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::String(value) => match value.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn value_to_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(value) => value.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn parse_font_size(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    let numeric = trimmed.strip_suffix("px").unwrap_or(trimmed).trim();
    numeric
        .parse::<f64>()
        .ok()
        .filter(|size| size.is_finite() && *size > 0.0)
}

fn frontmatter_bounds(source: &str) -> Option<(usize, usize, usize)> {
    let mut cursor = 0;
    while cursor < source.len() {
        let end = next_line_end(source, cursor);
        let line = source[cursor..end].trim();
        if line.is_empty() {
            cursor = end;
            continue;
        }
        if line != "---" {
            return None;
        }

        let yaml_start = end;
        let mut scan = end;
        while scan < source.len() {
            let scan_end = next_line_end(source, scan);
            if source[scan..scan_end].trim() == "---" {
                return Some((yaml_start, scan, scan_end));
            }
            scan = scan_end;
        }

        return None;
    }

    None
}

fn next_line_end(source: &str, start: usize) -> usize {
    source[start..]
        .find('\n')
        .map(|position| start + position + 1)
        .unwrap_or(source.len())
}
