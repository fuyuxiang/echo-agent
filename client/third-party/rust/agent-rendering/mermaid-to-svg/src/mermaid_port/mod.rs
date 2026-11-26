mod cluster_adjust;
mod dagre_layout_port;
mod flow_data;
mod flow_db;
mod flow_parser;

use crate::error::MermaidError;
use crate::theme::MermaidTheme;
use crate::RenderConfig;

pub fn render_mermaid_to_svg_ported(
    mermaid_source: &str,
    theme: &MermaidTheme,
    config: &RenderConfig,
) -> Result<String, MermaidError> {
    let graph = flow_parser::parse_flowchart(mermaid_source)?;
    let layout_result = dagre_layout_port::compute_layout_ported_with_config(&graph, config);
    Ok(crate::svg_renderer::render_with_config(
        &layout_result,
        theme,
        config,
    ))
}

// HERMETIC VENDORING PATCH: the experimental dagre flowchart "port" is disabled
// unconditionally. Upstream gated it on the `MERMAID_TO_SVG_USE_PORT` env var;
// reading the environment makes rendering non-deterministic over untrusted
// input, and the port mis-routes back-edges on cyclic flowcharts (detached
// arrowheads) — the exact defect this engine was adopted to fix. The default
// `layout::compute_layout` path routes cycles correctly.
pub fn is_enabled() -> bool {
    false
}

#[allow(dead_code)]
pub(crate) fn compute_layout_ported(
    flowchart: &crate::ast::FlowchartGraph,
) -> crate::layout::LayoutResult {
    dagre_layout_port::compute_layout_ported(flowchart)
}
