#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowchartGraph {
    pub direction: GraphDirection,
    pub statements: Vec<Statement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDirection {
    TopToBottom,
    BottomToTop,
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    Node(Node),
    Edge(Edge),
    Subgraph(Subgraph),
    Style(StyleStatement),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub label: Option<String>,
    pub shape: NodeShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeShape {
    Rectangle,
    RoundedRectangle,
    Stadium,
    Diamond,
    Hexagon,
    Asymmetric,
    Subroutine,
    Cylinder,
    Circle,
    StartState,
    EndState,
    ForkJoin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub style: EdgeStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeStyle {
    Arrow,
    Line,
    DottedArrow,
    DottedLine,
    ThickArrow,
    ThickLine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subgraph {
    pub id: String,
    pub title: Option<String>,
    pub statements: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleStatement {
    pub node_id: String,
    pub properties: Vec<(String, String)>,
}
