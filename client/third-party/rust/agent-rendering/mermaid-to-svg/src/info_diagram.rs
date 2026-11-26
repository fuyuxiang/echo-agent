use crate::error::MermaidError;
use crate::theme::MermaidTheme;

const INFO_WIDTH: f64 = 400.0;
const INFO_HEIGHT: f64 = 150.0;

const PINNED_MERMAID_VERSION: &str = "11.12.2";

pub fn render_info_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    if first_diagram_type_token(mermaid_source) != Some("info") {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'info' declaration".to_string(),
        });
    }

    let background_color = if theme.background == "#ffffff" {
        "white"
    } else {
        theme.background.as_str()
    };
    let text_color = if theme.text_color == "#333333" {
        "#333"
    } else {
        theme.text_color.as_str()
    };

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg aria-roledescription=\"info\" role=\"graphics-document document\" viewBox=\"0 0 {INFO_WIDTH} {INFO_HEIGHT}\" style=\"max-width: {INFO_WIDTH}px; background-color: {background_color};\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" id=\"my-svg\">"
    ));

    svg.push_str(&format!(
        "<style>#my-svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;fill:{text_color};}}@keyframes edge-animation-frame{{from{{stroke-dashoffset:0;}}}}@keyframes dash{{to{{stroke-dashoffset:0;}}}}#my-svg .edge-animation-slow{{stroke-dasharray:9,5!important;stroke-dashoffset:900;animation:dash 50s linear infinite;stroke-linecap:round;}}#my-svg .edge-animation-fast{{stroke-dasharray:9,5!important;stroke-dashoffset:900;animation:dash 20s linear infinite;stroke-linecap:round;}}#my-svg .error-icon{{fill:#552222;}}#my-svg .error-text{{fill:#552222;stroke:#552222;}}#my-svg .edge-thickness-normal{{stroke-width:1px;}}#my-svg .edge-thickness-thick{{stroke-width:3.5px;}}#my-svg .edge-pattern-solid{{stroke-dasharray:0;}}#my-svg .edge-thickness-invisible{{stroke-width:0;fill:none;}}#my-svg .edge-pattern-dashed{{stroke-dasharray:3;}}#my-svg .edge-pattern-dotted{{stroke-dasharray:2;}}#my-svg .marker{{fill:{edge};stroke:{edge};}}#my-svg .marker.cross{{stroke:{edge};}}#my-svg svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;}}#my-svg p{{margin:0;}}#my-svg :root{{--mermaid-font-family:\"trebuchet ms\",verdana,arial,sans-serif;}}</style>",
        edge = theme.edge_color
    ));

    svg.push_str("<g/>");
    svg.push_str(&format!(
        "<g><text style=\"text-anchor: middle;\" font-size=\"32\" class=\"version\" y=\"40\" x=\"100\">v{PINNED_MERMAID_VERSION}</text></g>"
    ));
    svg.push_str("</svg>");

    Ok(svg)
}

fn first_diagram_type_token(input: &str) -> Option<&str> {
    input
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("%%"))
        .and_then(|l| l.split_whitespace().next())
}
