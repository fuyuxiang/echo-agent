use thiserror::Error;

#[derive(Error, Debug)]
pub enum MermaidError {
    #[error("Parse error at line {line}: {message}")]
    ParseError { line: usize, message: String },

    #[error("Invalid graph direction: {0}")]
    InvalidDirection(String),

    #[error("Invalid node shape: {0}")]
    InvalidNodeShape(String),

    #[error("DOT generation error: {0}")]
    DotGenerationError(String),

    #[error("SVG rendering error: {0}")]
    RenderError(String),

    #[error("Unsupported diagram type: {0}")]
    UnsupportedDiagramType(String),
}
