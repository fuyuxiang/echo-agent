use crate::ast::FlowchartGraph;
use crate::error::MermaidError;

pub fn parse_flowchart(mermaid_source: &str) -> Result<FlowchartGraph, MermaidError> {
    crate::parser::parse_mermaid(mermaid_source)
}
